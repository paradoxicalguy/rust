#![feature(fn_delegation)]

struct S;

trait Trait<const Self: usize = 2> {
    //~^ ERROR expected identifier, found keyword `Self`
    fn foo();
}

impl S {
    reuse Trait::<_>::foo;
    //~^ ERROR the placeholder `_` is not allowed within types on item signatures for associated functions
}

fn main() {}
