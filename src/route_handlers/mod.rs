mod query_params;
use query_params::GetObjectQuery;

use crate::{apis::source::SourceAPI, utils::context::RequestContext};

use actix_web::{http::header, web, HttpRequest, HttpResponse, Responder};

pub async fn get_object(
    req: HttpRequest,
    query: web::Query<GetObjectQuery>,
    path: web::Path<(String, String, String)>,
    api_client: web::Data<SourceAPI>,
    ctx: web::ReqData<RequestContext>,
) -> impl Responder {
    // Get the account_id, repository_id, and key from the path
    let (account_id, repository_id, key) = path.into_inner();

    // If present, get the range header from the request
    let headers = req.headers();
    let mut range = None;
    let mut range_start = 0;
    let mut range_end = 0;
    let mut is_range_request = false;

    if let Some(range_header) = headers.get(header::RANGE) {
        if let Ok(r) = range_header.to_str() {
            if let Some(bytes_range) = r.strip_prefix("bytes=") {
                if let Some((start, end)) = bytes_range.split_once('-') {
                    if let (Ok(s), Ok(e)) = (start.parse::<u64>(), end.parse::<u64>()) {
                        range_start = s;
                        range_end = e;
                        range = Some(r.to_string());
                        is_range_request = true;
                    }
                }
            }
        }
    }

    HttpResponse::Ok()

    /*

    if let Ok(client) = api_client
        .get_backend_client(&account_id, &repository_id)
        .await
    {
        match api_client
            .is_authorized(
                user_identity.into_inner(),
                &account_id,
                &repository_id,
                RepositoryPermission::Read,
            )
            .await
        {
            Ok(authorized) => {
                if !authorized {
                    return HttpResponse::Unauthorized().finish();
                }
            }
            Err(_) => return HttpResponse::InternalServerError().finish(),
        }

        // Found the repository, now try to get the object
        match client.get_object(key.clone(), range).await {
            Ok(res) => {
                let mut content_length = String::from("*");

                // Remove this if statement to increase performance since it's making an extra request just to get the total content-length
                // This is only needed for range requests and in theory, you can return a * in the Content-Range header to indicate that the content length is unknown
                if is_range_request {
                    match client.head_object(key.clone()).await {
                        Ok(head_res) => {
                            content_length = head_res.content_length.to_string();
                        }
                        Err(_) => {}
                    }
                }

                let stream = res.body.map(|result| {
                    result
                        .map(web::Bytes::from)
                        .map_err(|e| ErrorInternalServerError(e.to_string()))
                });

                let streaming_response = StreamingResponse::new(stream, res.content_length);
                let mut response = if is_range_request {
                    HttpResponse::PartialContent()
                } else {
                    HttpResponse::Ok()
                };

                let mut response = response
                    .insert_header(("Content-Type", res.content_type))
                    .insert_header(("Last-Modified", res.last_modified))
                    .insert_header(("Content-Length", res.content_length.to_string()))
                    .insert_header(("ETag", res.etag));

                if is_range_request {
                    response = response.insert_header((
                        "Content-Range",
                        format!("bytes {}-{}/{}", range_start, range_end, content_length),
                    ));
                }

                return response.body(streaming_response);
            }
            Err(_) => HttpResponse::NotFound().finish(),
        }
    } else {
        // Could not find the repository
        return HttpResponse::NotFound().finish();
    } */
}
