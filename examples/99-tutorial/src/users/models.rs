use lesto::prelude::*;

#[derive(Deserialize, JsonSchema, Validate)]
pub struct UserIn {
    #[garde(length(min = 1))]
    pub name: String,
    #[garde(length(min = 8))]
    pub password: String,
}

#[derive(Clone, Serialize, JsonSchema)]
pub struct UserOut {
    pub id: u64,
    pub name: String,
}
