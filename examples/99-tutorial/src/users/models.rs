#[lesto::model]
pub struct UserIn {
    #[garde(length(min = 1))]
    pub name: String,
    #[garde(length(min = 8))]
    pub password: String,
}

#[lesto::model]
#[derive(Clone)]
pub struct UserOut {
    pub id: u64,
    pub name: String,
}
