use multistore::api::list::ListQueryParams;

/// Result of applying pagination to a list of prefixes.
pub struct PaginatedPrefixes {
    pub prefixes: Vec<String>,
    pub is_truncated: bool,
    pub next_continuation_token: Option<String>,
}

/// Apply S3-style pagination to a sorted list of prefix strings.
///
/// Prefixes are sorted lexicographically, then filtered by `start_after` /
/// `continuation_token`, and sliced to `max_keys`.
// TODO: Replace client-side pagination with real server-side pagination
// when the upstream Source Coop API supports paginated product listing.
pub fn paginate_prefixes(mut prefixes: Vec<String>, params: &ListQueryParams) -> PaginatedPrefixes {
    prefixes.sort();

    // continuation-token takes precedence over start-after (per S3 spec)
    let skip_after = params
        .continuation_token
        .as_deref()
        .or(params.start_after.as_deref());

    let iter: Box<dyn Iterator<Item = String>> = if let Some(after) = skip_after {
        Box::new(prefixes.into_iter().filter(move |p| p.as_str() > after))
    } else {
        Box::new(prefixes.into_iter())
    };

    let collected: Vec<String> = iter.take(params.max_keys + 1).collect();
    let is_truncated = collected.len() > params.max_keys;

    let mut result: Vec<String> = collected;
    if is_truncated {
        result.truncate(params.max_keys);
    }

    let next_continuation_token = if is_truncated {
        result.last().cloned()
    } else {
        None
    };

    PaginatedPrefixes {
        prefixes: result,
        is_truncated,
        next_continuation_token,
    }
}
