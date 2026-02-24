use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DoctorCheck {
    pub name: String,
    pub ok: bool,
    pub details: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DoctorCheckResult {
    Healthy,
    Degraded,
    Unhealthy,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DoctorReport {
    pub result: DoctorCheckResult,
    pub checks: Vec<DoctorCheck>,
}

impl DoctorReport {
    pub fn run() -> Self {
        let checks = vec![
            DoctorCheck {
                name: "cwd_resolvable".to_string(),
                ok: std::env::current_dir().is_ok(),
                details: "current working directory available".to_string(),
            },
            DoctorCheck {
                name: "project_root_env".to_string(),
                ok: std::env::var("PROJECT_ROOT").is_ok(),
                details: "PROJECT_ROOT is set (optional)".to_string(),
            },
        ];

        let failed = checks.iter().filter(|check| !check.ok).count();
        let result = match failed {
            0 => DoctorCheckResult::Healthy,
            1 => DoctorCheckResult::Degraded,
            _ => DoctorCheckResult::Unhealthy,
        };

        Self { result, checks }
    }
}
