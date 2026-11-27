pub mod outer {
    pub mod inner {
        #[must_use]
        pub struct Important;
    }
}

pub fn example() {
    outer::inner::Important;
}
