// Chapter 7: converting domain errors
use lesto::prelude::*;

#[allow(dead_code)]
pub enum RepoError {
    NotFound,
    Conflict(String),
    Db(String),
}

impl From<RepoError> for HttpError {
    fn from(e: RepoError) -> Self {
        match e {
            RepoError::NotFound => HttpError::not_found("resource not found"),
            RepoError::Conflict(msg) => HttpError::conflict(msg),
            RepoError::Db(_err) => HttpError::internal("internal error"),
        }
    }
}

pub fn out_of_credit(balance: u32, cost: u32) -> HttpError {
    HttpError::new(
        402,
        format!("Your current balance is {balance}, but that costs {cost}."),
    )
    .with_type("https://example.com/probs/out-of-credit")
    .with_title("You do not have enough credit.")
    .with_extension("balance", balance)
    .with_extension("cost", cost)
}
