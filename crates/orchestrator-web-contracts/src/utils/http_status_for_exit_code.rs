use http::StatusCode;

pub fn http_status_for_exit_code(exit_code: i32) -> StatusCode {
    match exit_code {
        2 => StatusCode::BAD_REQUEST,
        3 => StatusCode::NOT_FOUND,
        4 => StatusCode::CONFLICT,
        5 => StatusCode::SERVICE_UNAVAILABLE,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    }
}
