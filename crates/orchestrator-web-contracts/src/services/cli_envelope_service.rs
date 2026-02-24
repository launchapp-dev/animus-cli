use serde::Serialize;

use crate::models::{CliErrorBody, CliErrorEnvelope, CliSuccessEnvelope};

pub struct CliEnvelopeService;

impl CliEnvelopeService {
    pub const CLI_SCHEMA: &'static str = "ao.cli.v1";

    pub fn ok<T: Serialize>(data: T) -> CliSuccessEnvelope<T> {
        CliSuccessEnvelope {
            schema: Self::CLI_SCHEMA,
            ok: true,
            data,
        }
    }

    pub fn error(
        code: impl Into<String>,
        message: impl Into<String>,
        exit_code: i32,
    ) -> CliErrorEnvelope {
        CliErrorEnvelope {
            schema: Self::CLI_SCHEMA,
            ok: false,
            error: CliErrorBody {
                code: code.into(),
                message: message.into(),
                exit_code,
            },
        }
    }
}
