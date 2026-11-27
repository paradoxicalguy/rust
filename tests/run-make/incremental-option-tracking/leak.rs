pub fn example(
    x: for<'a, 'b> fn(&'a u8, &'b u8) -> &'a u8,
    y: for<'a> fn(&'a u8, &'a u8) -> &'a u8,
) {
    let _z = match 22 {
        0 => y,
        _ => x,
    };
}
