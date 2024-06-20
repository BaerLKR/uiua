//! Algorithms for tabling modifiers

use ecow::eco_vec;

use crate::{
    algorithm::{pervade::*, zip::rows1},
    function::Function,
    random,
    value::Value,
    Array, ArrayValue, Complex, ImplPrimitive, Instr, Primitive, Shape, Uiua, UiuaResult,
};

use super::{loops::flip, multi_output, reduce::reduce_impl, validate_size};

pub fn table(env: &mut Uiua) -> UiuaResult {
    let f = env.pop_function()?;
    table_impl(f, env)
}

fn table_impl(f: Function, env: &mut Uiua) -> UiuaResult {
    crate::profile_function!();
    let sig = f.signature();
    match sig.args {
        0 => env.call(f),
        1 => rows1(f, env.pop(1)?, false, env),
        n => {
            let xs = env.pop(1)?;
            let ys = env.pop(2)?;
            if n == 2 && xs.rank() <= 1 && ys.rank() <= 1 {
                table_list(f, xs, ys, env)
            } else {
                if let [Instr::Prim(Primitive::Mul, _), Instr::PushFunc(f), Instr::Prim(Primitive::Reduce, _)] =
                    f.instrs(&env.asm)
                {
                    if let Some((Primitive::Add, _)) = f.as_flipped_primitive(&env.asm) {
                        match (&xs, &ys) {
                            (Value::Num(a), Value::Num(b)) => {
                                return a.matrix_mul(b, env).map(|val| env.push(val))
                            }
                            (Value::Num(a), Value::Byte(b)) => {
                                return a.matrix_mul(&b.convert_ref(), env).map(|val| env.push(val))
                            }
                            (Value::Byte(a), Value::Num(b)) => {
                                return a.convert_ref().matrix_mul(b, env).map(|val| env.push(val))
                            }
                            (Value::Byte(a), Value::Byte(b)) => {
                                return a
                                    .convert_ref()
                                    .matrix_mul(&b.convert_ref(), env)
                                    .map(|val| env.push(val))
                            }
                            _ => {}
                        }
                    }
                }
                generic_table(f, xs, ys, env)
            }
        }
    }
}

fn generic_table(f: Function, xs: Value, ys: Value, env: &mut Uiua) -> UiuaResult {
    let sig = f.signature();
    match sig.args {
        2 => {
            validate_size::<f64>([sig.outputs, xs.row_count(), ys.row_count()], env)?;
            let new_shape = Shape::from([xs.row_count(), ys.row_count()]);
            let outputs = sig.outputs;
            let mut items = multi_output(outputs, Value::builder(xs.row_count() * ys.row_count()));
            let y_rows = ys.into_rows().collect::<Vec<_>>();
            env.without_fill(|env| -> UiuaResult {
                for x_row in xs.into_rows() {
                    for y_row in y_rows.iter().cloned() {
                        env.push(y_row);
                        env.push(x_row.clone());
                        env.call(f.clone())?;
                        for i in 0..outputs {
                            items[i].add_row(env.pop("tabled function result")?, env)?;
                        }
                    }
                }
                Ok(())
            })?;
            for items in items.into_iter().rev() {
                let mut tabled = items.finish();
                let mut new_shape = new_shape.clone();
                new_shape.extend_from_slice(&tabled.shape()[1..]);
                *tabled.shape_mut() = new_shape;
                tabled.validate_shape();
                env.push(tabled);
            }
        }
        n => {
            let zs = env.pop(3)?;
            let mut others = Vec::with_capacity(n - 3);
            for i in 3..n {
                others.push(env.pop(i + 1)?);
            }
            validate_size::<f64>(
                [
                    sig.outputs,
                    xs.row_count(),
                    ys.row_count(),
                    zs.row_count(),
                    others.iter().map(|a| a.row_count()).product::<usize>(),
                ],
                env,
            )?;
            let mut new_shape = Shape::with_capacity(n);
            for arg in [&xs, &ys, &zs].into_iter().chain(&others) {
                new_shape.push(arg.row_count());
            }
            let outputs = sig.outputs;
            let other_rows_product = others.iter().map(|a| a.row_count()).product::<usize>();
            let mut items = multi_output(
                outputs,
                Value::builder(
                    xs.row_count() * ys.row_count() * zs.row_count() * other_rows_product,
                ),
            );
            env.without_fill(|env| -> UiuaResult {
                for x_row in xs.into_rows() {
                    for y_row in ys.rows() {
                        for z_row in zs.rows() {
                            for mut i in 0..other_rows_product {
                                for arg in others.iter().rev() {
                                    let j = i % arg.row_count();
                                    env.push(arg.row(j));
                                    i /= arg.row_count();
                                }
                                env.push(z_row.clone());
                                env.push(y_row.clone());
                                env.push(x_row.clone());
                                env.call(f.clone())?;
                                for i in 0..outputs {
                                    items[i].add_row(env.pop("crossed function result")?, env)?;
                                }
                            }
                        }
                    }
                }
                Ok(())
            })?;
            for items in items.into_iter().rev() {
                let mut tabled = items.finish();
                let mut new_shape = new_shape.clone();
                new_shape.extend_from_slice(&tabled.shape()[1..]);
                *tabled.shape_mut() = new_shape;
                tabled.validate_shape();
                env.push(tabled);
            }
        }
    }
    Ok(())
}

pub fn table_list(f: Function, xs: Value, ys: Value, env: &mut Uiua) -> UiuaResult {
    crate::profile_function!();
    validate_size::<f64>([f.signature().outputs, xs.row_count(), ys.row_count()], env)?;
    match (f.as_flipped_primitive(&env.asm), xs, ys) {
        (Some((prim, flipped)), Value::Num(xs), Value::Num(ys)) => {
            if let Err((xs, ys)) = table_nums(prim, flipped, xs, ys, env)? {
                return generic_table(f, Value::Num(xs), Value::Num(ys), env);
            }
        }
        (Some((prim, flipped)), Value::Byte(xs), Value::Byte(ys)) => match prim {
            Primitive::Eq => env.push(fast_table_list(xs, ys, is_eq::generic, env)?),
            Primitive::Ne => env.push(fast_table_list(xs, ys, is_ne::generic, env)?),
            Primitive::Lt if flipped => {
                env.push(fast_table_list(xs, ys, flip(is_lt::generic), env)?)
            }
            Primitive::Lt => env.push(fast_table_list(xs, ys, is_lt::generic, env)?),
            Primitive::Gt if flipped => {
                env.push(fast_table_list(xs, ys, flip(is_gt::generic), env)?)
            }
            Primitive::Gt => env.push(fast_table_list(xs, ys, is_gt::generic, env)?),
            Primitive::Le if flipped => {
                env.push(fast_table_list(xs, ys, flip(is_le::generic), env)?)
            }
            Primitive::Le => env.push(fast_table_list(xs, ys, is_le::generic, env)?),
            Primitive::Ge if flipped => {
                env.push(fast_table_list(xs, ys, flip(is_ge::generic), env)?)
            }
            Primitive::Ge => env.push(fast_table_list(xs, ys, is_ge::generic, env)?),
            Primitive::Add => env.push(fast_table_list(xs, ys, add::byte_byte, env)?),
            Primitive::Sub if flipped => {
                env.push(fast_table_list(xs, ys, flip(sub::byte_byte), env)?)
            }
            Primitive::Sub => env.push(fast_table_list(xs, ys, sub::byte_byte, env)?),
            Primitive::Mul => env.push(fast_table_list(xs, ys, mul::byte_byte, env)?),
            Primitive::Div if flipped => {
                env.push(fast_table_list(xs, ys, flip(div::byte_byte), env)?)
            }
            Primitive::Div => env.push(fast_table_list(xs, ys, div::byte_byte, env)?),
            Primitive::Mod if flipped => {
                env.push(fast_table_list(xs, ys, flip(modulus::byte_byte), env)?)
            }
            Primitive::Mod => env.push(fast_table_list(xs, ys, modulus::byte_byte, env)?),
            Primitive::Atan if flipped => env.push(fast_table_list::<f64, _>(
                xs.convert(),
                ys.convert(),
                flip(atan2::num_num),
                env,
            )?),
            Primitive::Atan => env.push(fast_table_list::<f64, _>(
                xs.convert(),
                ys.convert(),
                atan2::num_num,
                env,
            )?),
            Primitive::Complex if flipped => {
                env.push(fast_table_list(xs, ys, flip(complex::byte_byte), env)?)
            }
            Primitive::Complex => env.push(fast_table_list(xs, ys, complex::byte_byte, env)?),
            Primitive::Min => env.push(fast_table_list(xs, ys, min::byte_byte, env)?),
            Primitive::Max => env.push(fast_table_list(xs, ys, max::byte_byte, env)?),
            Primitive::Join | Primitive::Couple => {
                env.push(fast_table_list_join_or_couple(xs, ys, flipped, env)?)
            }
            _ => generic_table(f, Value::Byte(xs), Value::Byte(ys), env)?,
        },

        (Some((prim, flipped)), Value::Complex(xs), Value::Complex(ys)) => {
            if let Err((xs, ys)) = table_coms(prim, flipped, xs, ys, env)? {
                return generic_table(f, Value::Complex(xs), Value::Complex(ys), env);
            }
        }
        (Some((prim, flipped)), Value::Num(xs), Value::Byte(ys)) => {
            let ys = ys.convert();
            if let Err((xs, ys)) = table_nums(prim, flipped, xs, ys, env)? {
                return generic_table(f, Value::Num(xs), Value::Num(ys), env);
            }
        }
        (Some((prim, flipped)), Value::Byte(xs), Value::Num(ys)) => {
            let xs = xs.convert();
            if let Err((xs, ys)) = table_nums(prim, flipped, xs, ys, env)? {
                return generic_table(f, Value::Num(xs), Value::Num(ys), env);
            }
        }

        (Some((prim, flipped)), Value::Num(xs), Value::Complex(ys)) => {
            let xs = xs.convert();
            if let Err((xs, ys)) = table_coms(prim, flipped, xs, ys, env)? {
                return generic_table(f, Value::Complex(xs), Value::Complex(ys), env);
            }
        }

        (Some((prim, flipped)), Value::Complex(xs), Value::Num(ys)) => {
            let ys = ys.convert();
            if let Err((xs, ys)) = table_coms(prim, flipped, xs, ys, env)? {
                return generic_table(f, Value::Complex(xs), Value::Complex(ys), env);
            }
        }
        // Boxes
        (Some((Primitive::Join | Primitive::Couple, flipped)), Value::Box(xs), ys) => env.push(
            fast_table_list_join_or_couple(xs, ys.coerce_to_boxes(), flipped, env)?,
        ),
        (Some((Primitive::Join | Primitive::Couple, flipped)), xs, Value::Box(ys)) => env.push(
            fast_table_list_join_or_couple(xs.coerce_to_boxes(), ys, flipped, env)?,
        ),
        // Chars
        (
            Some((Primitive::Join | Primitive::Couple, flipped)),
            Value::Char(xs),
            Value::Char(ys),
        ) => env.push(fast_table_list_join_or_couple(xs, ys, flipped, env)?),
        (_, xs, ys) => match f.as_flipped_impl_primitive(&env.asm) {
            // Random
            Some((ImplPrimitive::ReplaceRand2, _)) => {
                let shape = [xs.row_count(), ys.row_count()];
                let mut data = eco_vec![0.0; xs.row_count() * ys.row_count()];
                for n in data.make_mut() {
                    *n = random();
                }
                env.push(Array::new(shape, data));
            }
            _ => generic_table(f, xs, ys, env)?,
        },
    }
    Ok(())
}

macro_rules! table_math {
    ($fname:ident, $ty:ty, $f:ident) => {
        #[allow(clippy::result_large_err)]
        fn $fname(
            prim: Primitive,
            flipped: bool,
            xs: Array<$ty>,
            ys: Array<$ty>,
            env: &mut Uiua,
        ) -> UiuaResult<Result<(), (Array<$ty>, Array<$ty>)>> {
            match prim {
                Primitive::Eq => env.push(fast_table_list(xs, ys, is_eq::$f, env)?),
                Primitive::Ne => env.push(fast_table_list(xs, ys, is_ne::$f, env)?),
                Primitive::Lt if flipped => {
                    env.push(fast_table_list(xs, ys, flip(is_lt::$f), env)?)
                }
                Primitive::Lt => env.push(fast_table_list(xs, ys, is_lt::$f, env)?),
                Primitive::Gt if flipped => {
                    env.push(fast_table_list(xs, ys, flip(is_gt::$f), env)?)
                }
                Primitive::Gt => env.push(fast_table_list(xs, ys, is_gt::$f, env)?),
                Primitive::Le if flipped => {
                    env.push(fast_table_list(xs, ys, flip(is_le::$f), env)?)
                }
                Primitive::Le => env.push(fast_table_list(xs, ys, is_le::$f, env)?),
                Primitive::Ge if flipped => {
                    env.push(fast_table_list(xs, ys, flip(is_ge::$f), env)?)
                }
                Primitive::Ge => env.push(fast_table_list(xs, ys, is_ge::$f, env)?),
                Primitive::Add => env.push(fast_table_list(xs, ys, add::$f, env)?),
                Primitive::Sub if flipped => env.push(fast_table_list(xs, ys, flip(sub::$f), env)?),
                Primitive::Sub => env.push(fast_table_list(xs, ys, sub::$f, env)?),
                Primitive::Mul => env.push(fast_table_list(xs, ys, mul::$f, env)?),
                Primitive::Div if flipped => env.push(fast_table_list(xs, ys, flip(div::$f), env)?),
                Primitive::Div => env.push(fast_table_list(xs, ys, div::$f, env)?),
                Primitive::Mod if flipped => {
                    env.push(fast_table_list(xs, ys, flip(modulus::$f), env)?)
                }
                Primitive::Mod => env.push(fast_table_list(xs, ys, modulus::$f, env)?),
                Primitive::Atan if flipped => {
                    env.push(fast_table_list(xs, ys, flip(atan2::$f), env)?)
                }
                Primitive::Atan => env.push(fast_table_list(xs, ys, atan2::$f, env)?),

                Primitive::Complex if flipped => {
                    env.push(fast_table_list(xs, ys, flip(complex::$f), env)?)
                }

                Primitive::Complex => env.push(fast_table_list(xs, ys, complex::$f, env)?),
                Primitive::Min => env.push(fast_table_list(xs, ys, min::$f, env)?),
                Primitive::Max => env.push(fast_table_list(xs, ys, max::$f, env)?),
                Primitive::Join | Primitive::Couple => {
                    env.push(fast_table_list_join_or_couple(xs, ys, flipped, env)?)
                }
                _ => return Ok(Err((xs, ys))),
            }
            Ok(Ok(()))
        }
    };
}

table_math!(table_nums, f64, num_num);
table_math!(table_coms, crate::Complex, com_x);

fn fast_table_list<T: ArrayValue, U: ArrayValue + Default>(
    a: Array<T>,
    b: Array<T>,
    f: impl Fn(T, T) -> U,
    env: &Uiua,
) -> UiuaResult<Array<U>> {
    let elem_count = validate_size::<U>([a.data.len(), b.data.len()], env)?;
    let mut new_data = eco_vec![U::default(); elem_count];
    let data_slice = new_data.make_mut();
    let mut i = 0;
    for x in a.data {
        for y in b.data.iter().cloned() {
            data_slice[i] = f(x.clone(), y);
            i += 1;
        }
    }
    let mut new_shape = a.shape;
    new_shape.extend_from_slice(&b.shape);
    Ok(Array::new(new_shape, new_data))
}

fn fast_table_list_join_or_couple<T: ArrayValue + Default>(
    a: Array<T>,
    b: Array<T>,
    flipped: bool,
    env: &Uiua,
) -> UiuaResult<Array<T>> {
    let elem_count = validate_size::<T>([a.data.len(), b.data.len(), 2], env)?;
    let mut new_data = eco_vec![T::default(); elem_count];
    let data_slice = new_data.make_mut();
    let mut i = 0;
    if flipped {
        for x in a.data {
            for y in b.data.iter().cloned() {
                data_slice[i] = y;
                i += 1;
                data_slice[i] = x.clone();
                i += 1;
            }
        }
    } else {
        for x in a.data {
            for y in b.data.iter().cloned() {
                data_slice[i] = x.clone();
                i += 1;
                data_slice[i] = y;
                i += 1;
            }
        }
    }
    let mut new_shape = a.shape;
    new_shape.extend_from_slice(&b.shape);
    new_shape.push(2);
    Ok(Array::new(new_shape, new_data))
}

pub fn reduce_table(env: &mut Uiua) -> UiuaResult {
    let f = env.pop_function()?;
    let g = env.pop_function()?;
    let xs = env.pop(1)?;
    let ys = env.pop(2)?;
    if xs.rank() == 1 && ys.rank() == 1 {
        let prims = f
            .as_flipped_primitive(&env.asm)
            .zip(g.as_flipped_primitive(&env.asm));
        match (prims, xs, ys) {
            (Some(((fp, f_flip), (gp, g_flip))), Value::Num(xs), Value::Num(ys)) => {
                if let Err((xs, ys)) = reduce_table_nums(fp, gp, f_flip, g_flip, xs, ys, env)? {
                    return generic_reduce_table(f, g, Value::Num(xs), Value::Num(ys), env);
                }
            }
            (Some(((fp, f_flip), (gp, g_flip))), Value::Complex(xs), Value::Complex(ys)) => {
                if let Err((xs, ys)) = reduce_coms(fp, gp, f_flip, g_flip, xs, ys, env)? {
                    return generic_reduce_table(f, g, Value::Complex(xs), Value::Complex(ys), env);
                }
            }
            (Some(((fp, f_flip), (gp, g_flip))), Value::Byte(xs), Value::Num(ys)) => {
                let xs = xs.convert();
                if let Err((xs, ys)) = reduce_table_nums(fp, gp, f_flip, g_flip, xs, ys, env)? {
                    return generic_reduce_table(f, g, Value::Num(xs), Value::Num(ys), env);
                }
            }
            (Some(((fp, f_flip), (gp, g_flip))), Value::Num(xs), Value::Byte(ys)) => {
                let ys = ys.convert();
                if let Err((xs, ys)) = reduce_table_nums(fp, gp, f_flip, g_flip, xs, ys, env)? {
                    return generic_reduce_table(f, g, Value::Num(xs), Value::Num(ys), env);
                }
            }
            (Some(((fp, false), (gp, false))), Value::Byte(xs), Value::Byte(ys)) => {
                if let Err((xs, ys)) = reduce_table_bytes(fp, gp, xs, ys, env) {
                    return generic_reduce_table(f, g, Value::Byte(xs), Value::Byte(ys), env);
                }
            }
            (_, xs, ys) => generic_reduce_table(f, g, xs, ys, env)?,
        }
    } else {
        generic_reduce_table(f, g, xs, ys, env)?;
    }
    Ok(())
}

fn reduce_table_bytes(
    fp: Primitive,
    gp: Primitive,
    xs: Array<u8>,
    ys: Array<u8>,
    env: &mut Uiua,
) -> Result<(), (Array<u8>, Array<u8>)> {
    macro_rules! all_gs {
        ($xs:expr, $ys:expr, $ff:expr, $ff_complex:expr, $iden:expr, $ciden:expr, $fill:expr, $arith:ident, $cmp:ident) => {{
            let fill = $fill.map(Into::into);
            match gp {
                Primitive::Add => env.push(frtl($xs, $ys, $ff, add::$arith, $iden, fill)),
                Primitive::Sub => env.push(frtl($xs, $ys, $ff, sub::$arith, $iden, fill)),
                Primitive::Mul => env.push(frtl($xs, $ys, $ff, mul::$arith, $iden, fill)),
                Primitive::Div => env.push(frtl($xs, $ys, $ff, div::$arith, $iden, fill)),
                Primitive::Mod => env.push(frtl($xs, $ys, $ff, modulus::$arith, $iden, fill)),
                Primitive::Atan => env.push(frtl($xs, $ys, $ff, atan2::$arith, $iden, fill)),
                Primitive::Eq => env.push(frtl($xs, $ys, $ff, to(is_eq::$cmp), $iden, fill)),
                Primitive::Ne => env.push(frtl($xs, $ys, $ff, to(is_ne::$cmp), $iden, fill)),
                Primitive::Lt => env.push(frtl($xs, $ys, $ff, to(is_lt::$cmp), $iden, fill)),
                Primitive::Gt => env.push(frtl($xs, $ys, $ff, to(is_gt::$cmp), $iden, fill)),
                Primitive::Le => env.push(frtl($xs, $ys, $ff, to(is_le::$cmp), $iden, fill)),
                Primitive::Ge => env.push(frtl($xs, $ys, $ff, to(is_ge::$cmp), $iden, fill)),
                Primitive::Min => env.push(frtl($xs, $ys, $ff, min::$arith, $iden, fill)),
                Primitive::Max => env.push(frtl($xs, $ys, $ff, max::$arith, $iden, fill)),
                Primitive::Complex => env.push(frtl(
                    $xs,
                    $ys,
                    $ff_complex,
                    complex::$arith,
                    Complex::new($iden, $ciden),
                    env.complex_scalar_fill().ok(),
                )),
                Primitive::Couple | Primitive::Join => env.push(frtljc($xs, $ys, $ff, $iden, fill)),
                _ => return Err((xs, ys)),
            }
        }};
    }
    let fill = env.num_scalar_fill().ok();
    match fp {
        Primitive::Add => {
            all_gs!(
                xs,
                ys,
                to_left(add::num_num),
                add::com_x,
                0.0,
                0.0,
                fill,
                byte_byte,
                generic
            )
        }
        Primitive::Mul => {
            all_gs!(
                xs,
                ys,
                to_left(mul::num_num),
                mul::com_x,
                1.0,
                0.0,
                fill,
                byte_byte,
                generic
            )
        }
        Primitive::Min => {
            let byte_fill = env.byte_scalar_fill().ok();
            if xs.row_count() == 0 || fill.is_some() && byte_fill.is_none() {
                all_gs!(
                    xs.convert(),
                    ys.convert(),
                    min::num_num,
                    min::com_x,
                    f64::INFINITY,
                    f64::INFINITY,
                    fill,
                    num_num,
                    num_num
                )
            } else {
                all_gs!(
                    xs,
                    ys,
                    to_left(min::num_num),
                    min::com_x,
                    f64::INFINITY,
                    f64::INFINITY,
                    byte_fill,
                    byte_byte,
                    generic
                )
            }
        }
        Primitive::Max => {
            let byte_fill = env.byte_scalar_fill().ok();
            if xs.row_count() == 0 || fill.is_some() && byte_fill.is_none() {
                all_gs!(
                    xs.convert(),
                    ys.convert(),
                    max::num_num,
                    max::com_x,
                    f64::NEG_INFINITY,
                    f64::NEG_INFINITY,
                    fill,
                    num_num,
                    num_num
                )
            } else {
                all_gs!(
                    xs,
                    ys,
                    to_left(max::num_num),
                    max::com_x,
                    f64::NEG_INFINITY,
                    f64::NEG_INFINITY,
                    byte_fill,
                    byte_byte,
                    generic
                )
            }
        }
        _ => return Err((xs, ys)),
    }
    Ok(())
}

fn generic_reduce_table(
    f: Function,
    g: Function,
    xs: Value,
    ys: Value,
    env: &mut Uiua,
) -> UiuaResult {
    if env.value_fill().is_some() {
        env.push(ys);
        env.push(xs);
        table_impl(g, env)?;
        return reduce_impl(f, 0, env);
    }

    let mut xs = xs.into_rows();
    let mut acc = xs
        .next()
        .ok_or_else(|| env.error("Cannot reduce empty array"))?;
    let mut g_rows = Value::builder(ys.row_count());
    for y in ys.rows() {
        env.push(y);
        env.push(acc.clone());
        env.call(g.clone())?;
        g_rows.add_row(env.pop("reduced function result")?, env)?;
    }
    acc = g_rows.finish();
    for x in xs {
        g_rows = Value::builder(ys.row_count());
        for y in ys.rows() {
            env.push(y);
            env.push(x.clone());
            env.call(g.clone())?;
            g_rows.add_row(env.pop("reduced function result")?, env)?;
        }
        env.push(g_rows.finish());
        env.push(acc);
        env.call(f.clone())?;
        acc = env.pop("reduced function result")?;
    }
    env.push(acc);
    Ok(())
}

fn to<T, U>(f: impl Fn(T, T) -> U) -> impl Fn(T, T) -> T
where
    U: Into<T>,
{
    move |a, b| f(a, b).into()
}

fn to_left<T, U>(f: impl Fn(T, T) -> T) -> impl Fn(T, U) -> T
where
    U: Into<T>,
{
    move |a, b| f(a, b.into())
}

macro_rules! reduce_table_math {
    ($fname:ident, $ty:ty, $f:ident, $fill:ident) => {
        #[allow(clippy::result_large_err)]
        fn $fname(
            f_prim: Primitive,
            g_prim: Primitive,
            f_flipped: bool,
            g_flipped: bool,
            xs: Array<$ty>,
            ys: Array<$ty>,
            env: &mut Uiua,
        ) -> UiuaResult<Result<(), (Array<$ty>, Array<$ty>)>> {
            if f_flipped || g_flipped {
                return Ok(Err((xs, ys)));
            }
            let fill = env.$fill().ok();
            macro_rules! all_gs {
                ($ff:expr, $ff_complex:expr, $iden:expr, $ciden:expr) => {
                    match g_prim {
                        Primitive::Add => env.push(frtl(xs, ys, $ff, add::$f, $iden.into(), fill)),
                        Primitive::Sub => env.push(frtl(xs, ys, $ff, sub::$f, $iden.into(), fill)),
                        Primitive::Mul => env.push(frtl(xs, ys, $ff, mul::$f, $iden.into(), fill)),
                        Primitive::Div => env.push(frtl(xs, ys, $ff, div::$f, $iden.into(), fill)),
                        Primitive::Mod => {
                            env.push(frtl(xs, ys, $ff, modulus::$f, $iden.into(), fill))
                        }
                        Primitive::Atan => {
                            env.push(frtl(xs, ys, $ff, atan2::$f, $iden.into(), fill))
                        }
                        Primitive::Eq => {
                            env.push(frtl(xs, ys, $ff, to(is_eq::$f), $iden.into(), fill))
                        }
                        Primitive::Ne => {
                            env.push(frtl(xs, ys, $ff, to(is_ne::$f), $iden.into(), fill))
                        }
                        Primitive::Lt => {
                            env.push(frtl(xs, ys, $ff, to(is_lt::$f), $iden.into(), fill))
                        }
                        Primitive::Gt => {
                            env.push(frtl(xs, ys, $ff, to(is_gt::$f), $iden.into(), fill))
                        }
                        Primitive::Le => {
                            env.push(frtl(xs, ys, $ff, to(is_le::$f), $iden.into(), fill))
                        }
                        Primitive::Ge => {
                            env.push(frtl(xs, ys, $ff, to(is_ge::$f), $iden.into(), fill))
                        }
                        Primitive::Min => env.push(frtl(xs, ys, $ff, min::$f, $iden.into(), fill)),
                        Primitive::Max => env.push(frtl(xs, ys, $ff, max::$f, $iden.into(), fill)),
                        Primitive::Complex => env.push(frtl(
                            xs,
                            ys,
                            $ff_complex,
                            complex::$f,
                            Complex::new($iden, $ciden),
                            env.complex_scalar_fill().ok(),
                        )),
                        Primitive::Couple | Primitive::Join => {
                            env.push(frtljc(xs, ys, $ff, $iden.into(), fill))
                        }
                        _ => return Ok(Err((xs, ys))),
                    }
                };
            }
            match f_prim {
                Primitive::Add => all_gs!(add::$f, add::com_x, 0.0, 0.0),
                Primitive::Mul => all_gs!(mul::$f, mul::com_x, 1.0, 0.0),
                Primitive::Min => all_gs!(min::$f, min::com_x, f64::INFINITY, f64::INFINITY),
                Primitive::Max => {
                    all_gs!(max::$f, max::com_x, f64::NEG_INFINITY, f64::NEG_INFINITY)
                }
                _ => return Ok(Err((xs, ys))),
            }
            Ok(Ok(()))
        }
    };
}

reduce_table_math!(reduce_table_nums, f64, num_num, num_scalar_fill);
reduce_table_math!(reduce_coms, Complex, com_x, complex_scalar_fill);

/// Fast reduce table list
fn frtl<T, G, F>(
    a: Array<T>,
    b: Array<T>,
    f: impl Fn(F, G) -> F,
    g: impl Fn(T, T) -> G,
    identity: F,
    default: Option<F>,
) -> Array<F>
where
    T: ArrayValue + Copy,
    G: ArrayValue,
    F: ArrayValue + Copy,
{
    let mut acc = eco_vec![default.unwrap_or(identity); b.shape().elements()];
    let acc_slice = acc.make_mut();
    for a in a.data {
        for (&b, c) in b.data.iter().zip(&mut *acc_slice) {
            *c = f(*c, g(a, b));
        }
    }
    Array::new(b.shape, acc)
}

/// Fast reduce table list join or couple
fn frtljc<T, F>(
    a: Array<T>,
    b: Array<T>,
    f: impl Fn(F, T) -> F,
    identity: F,
    default: Option<F>,
) -> Array<F>
where
    T: ArrayValue + Copy,
    F: ArrayValue + Copy,
{
    let mut acc = eco_vec![default.unwrap_or(identity); b.shape().elements() * 2];
    let acc_slice = acc.make_mut();
    for a in a.data {
        let mut i = 0;
        for b in b.data.iter().cloned() {
            acc_slice[i] = f(acc_slice[i], a);
            i += 1;
            acc_slice[i] = f(acc_slice[i], b);
            i += 1;
        }
    }
    let mut new_shape = b.shape.clone();
    new_shape.push(2);
    Array::new(new_shape, acc)
}

pub fn triangle(env: &mut Uiua) -> UiuaResult {
    let f = env.pop_function()?;
    match f.signature().args {
        0 => env.call(f),
        1 => triangle1(f, env),
        2 => triangle2(f, env),
        3 => triangle3(f, env),
        _ => Err(env.error(format!(
            "{} of more that 3 arrays is not supported",
            Primitive::Triangle
        ))),
    }
}

fn triangle1(f: Function, env: &mut Uiua) -> UiuaResult {
    let xs = env.pop(1)?;
    let outputs = f.signature().outputs;
    match &**xs.shape() {
        [] => {
            env.push(xs);
            env.call(f)
        }
        [_] => {
            if let Some(Primitive::First) = f.as_primitive(&env.asm) {
                env.push(xs);
                return Ok(());
            }
            let rows = (0..xs.row_count()).map(|r| {
                let mut row = xs.clone();
                row.drop_n(r);
                row
            });
            let outputs = f.signature().outputs;
            let mut new_values = multi_output(outputs, Vec::new());
            env.without_fill(|env| -> UiuaResult {
                for row in rows {
                    env.push(row);
                    env.call(f.clone())?;
                    for i in 0..outputs {
                        new_values[i].push(env.pop("triangle's function result")?);
                    }
                }
                Ok(())
            })?;
            for values in new_values.into_iter().rev() {
                env.push(Value::from_row_values(values, env)?);
            }
            Ok(())
        }
        &[_, second_dim, ..] => {
            let rows = xs
                .into_rows()
                .take(second_dim)
                .enumerate()
                .map(|(r, mut row)| {
                    row.drop_n(r);
                    row
                });
            if let Some(Primitive::First) = f.as_primitive(&env.asm) {
                let value = Value::from_row_values_infallible(rows.map(|row| row.row(0)));
                env.push(value);
                return Ok(());
            }
            let mut new_values = multi_output(outputs, Vec::new());
            env.without_fill(|env| -> UiuaResult {
                for row in rows {
                    env.push(row);
                    env.call(f.clone())?;
                    for i in 0..outputs {
                        new_values[i].push(env.pop("triangle's function result")?);
                    }
                }
                Ok(())
            })?;
            for values in new_values.into_iter().rev() {
                env.push(Value::from_row_values(values, env)?);
            }
            Ok(())
        }
    }
}

fn triangle2(f: Function, env: &mut Uiua) -> UiuaResult {
    let mut xs = env.pop(1)?;
    let mut ys = env.pop(2)?;
    let both_scalar = xs.rank() == 0 && ys.rank() == 0;
    if xs.rank() == 0 {
        xs.fix();
    }
    if ys.rank() == 0 {
        ys.fix();
    }
    let outputs = f.signature().outputs;
    let new_values = env.without_fill(|env| -> UiuaResult<_> {
        match f.as_primitive(&env.asm) {
            Some(Primitive::Join) => {
                let mut new_rows = Vec::new();
                for x in xs.into_rows().take(ys.row_count()) {
                    for y in ys.rows() {
                        let row = x.clone().join(y, true, env)?;
                        new_rows.push(row);
                    }
                    ys.drop_n(1);
                }
                let mut val = Value::from_row_values(new_rows, env)?;
                if both_scalar {
                    val.shape_mut().remove(0);
                }
                env.push(val);
                return Ok(None);
            }
            Some(Primitive::Couple) => {
                let mut new_rows = Vec::new();
                for x in xs.into_rows().take(ys.row_count()) {
                    for y in ys.rows() {
                        let row = x.clone().couple(y, env)?;
                        new_rows.push(row);
                    }
                    ys.drop_n(1);
                }
                let mut val = Value::from_row_values(new_rows, env)?;
                if both_scalar {
                    val.shape_mut().remove(0);
                }
                env.push(val);
                return Ok(None);
            }
            _ => {}
        }
        let mut new_values = multi_output(outputs, Vec::new());
        for x in xs.into_rows().take(ys.row_count()) {
            for y in ys.rows() {
                env.push(y);
                env.push(x.clone());
                env.call(f.clone())?;
                for i in 0..outputs {
                    new_values[i].push(env.pop("triangle's function result")?);
                }
            }
            ys.drop_n(1);
        }
        Ok(Some(new_values))
    })?;
    if let Some(new_values) = new_values {
        for values in new_values.into_iter().rev() {
            let mut val = Value::from_row_values(values, env)?;
            if both_scalar {
                val.shape_mut().remove(0);
            }
            env.push(val);
        }
    }
    Ok(())
}

fn triangle3(f: Function, env: &mut Uiua) -> UiuaResult {
    let sig = f.signature();
    let xs = env.pop(1)?;
    let ys = env.pop(2)?;
    let zs = env.pop(3)?;
    let outputs = sig.outputs;
    let mut new_values = multi_output(
        outputs,
        Vec::with_capacity(xs.row_count() * ys.row_count() * zs.row_count() / 2),
    );
    env.without_fill(|env| -> UiuaResult {
        for (i, x) in xs.into_rows().take(ys.row_count()).enumerate() {
            for (j, y) in ys.rows().take(zs.row_count()).enumerate() {
                for (k, z) in zs.rows().enumerate() {
                    if i + j > k {
                        continue;
                    }
                    env.push(z);
                    env.push(y.clone());
                    env.push(x.clone());
                    env.call(f.clone())?;
                    for i in 0..outputs {
                        new_values[i].push(env.pop("triangle's function result")?);
                    }
                }
            }
        }
        Ok(())
    })?;
    for values in new_values.into_iter().rev() {
        env.push(Value::from_row_values(values, env)?);
    }
    Ok(())
}
