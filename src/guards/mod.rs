use actix_web::guard::{Guard, GuardContext};
use std::collections::HashMap;
use url::form_urlencoded;

pub struct ListObjectsV2Guard;

impl Guard for ListObjectsV2Guard {
    fn check(&self, ctx: &GuardContext) -> bool {
        let query = ctx.head().uri.query().unwrap_or("");
        let params: HashMap<String, String> = form_urlencoded::parse(query.as_bytes())
            .into_owned()
            .collect();

        params.contains_key("foo")
    }
}

pub struct GetObjectGuard;

impl Guard for GetObjectGuard {
    fn check(&self, ctx: &GuardContext) -> bool {
        return true;
    }
}
