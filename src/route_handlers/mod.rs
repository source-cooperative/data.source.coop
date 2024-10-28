mod query_params;
use query_params::GetObjectQuery;

use crate::utils::errors::{APIError, BadRequestError};
use crate::{
    apis::source::{RepositoryPermission, SourceAPI},
    utils::context::RequestContext,
};

use actix_web::{http::header, web, HttpRequest, HttpResponse, Responder};

pub async fn get_object(
    req: HttpRequest,
    query: web::Query<GetObjectQuery>,
    path: web::Path<(String, String, String)>,
    api_client: web::Data<SourceAPI>,
    ctx: web::ReqData<RequestContext>,
) -> impl Responder {
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

    let client = ctx.client.as_ref().unwrap();

    match api_client
        .is_authorized(&ctx, RepositoryPermission::Read)
        .await
    {
        Ok(authorized) => {
            if !authorized {
                let res: BadRequestError = BadRequestError {
                    message: "Unauthorized to read the repository".to_string(),
                };
                return res.to_response();
            }
        }
        Err(e) => return e.to_response(),
    }

    match client.get_object(ctx.key.unwrap().clone(), range).await {
        Ok(res) => {
            let mut content_length = String::from("*");

            // Remove this if statement to increase performance since it's making an extra request just to get the total content-length
            // This is only needed for range requests and in theory, you can return a * in the Content-Range header to indicate that the content length is unknown
            if is_range_request {
                match client.head_object(ctx.key.unwrap().clone()).await {
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
}
