// `#[lesto::model]` takes only `views(..)`.
#[lesto::model(view(Create(title)))]
struct Book {
    title: String,
}

fn main() {}
