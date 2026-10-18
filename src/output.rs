//! Diagnostics reporting shared by every subcommand.

use crate::parser::{Finding, Severity};
use crate::util::{EXIT_OK, EXIT_PARSE_FAILED, EXIT_POLICY_FAILED, json_escape};

/// Print parse findings; exit 4 if any is an Error.
pub fn report(findings: &[Finding], label: &str) -> i32 {
    let mut failed = false;
    for f in findings {
        match f.severity {
            Severity::Error => {
                failed = true;
                match f.line {
                    Some(n) => eprintln!("envwrap: {label}:line {n}: {}", f.message),
                    None => eprintln!("envwrap: {label}: {}", f.message),
                }
            }
            Severity::Warning => match f.line {
                Some(n) => eprintln!("envwrap: warning: {label}:line {n}: {}", f.message),
                None => eprintln!("envwrap: warning: {label}: {}", f.message),
            },
        }
    }
    if failed { EXIT_PARSE_FAILED } else { EXIT_OK }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PolicyFinding {
    pub id: &'static str,
    pub severity: Severity,
    pub variable: String,
    pub message: String,
}

#[derive(Debug, Default)]
pub struct PolicyReport {
    pub findings: Vec<PolicyFinding>,
    pub counts: Counts,
}

#[derive(Debug, Default)]
pub struct Counts {
    pub variables: usize,
    pub mirrors: usize,
    pub proxy_checks: usize,
}

impl PolicyReport {
    pub fn has_error(&self) -> bool {
        self.findings.iter().any(|f| f.severity == Severity::Error)
    }

    pub fn push(
        &mut self,
        id: &'static str,
        severity: Severity,
        variable: impl Into<String>,
        message: impl Into<String>,
    ) {
        self.findings.push(PolicyFinding {
            id,
            severity,
            variable: variable.into(),
            message: message.into(),
        });
    }
}

/// Print a policy report; exit 3 if any finding is an Error.
pub fn policy(report: &PolicyReport, json: bool) -> i32 {
    let code = if report.has_error() {
        EXIT_POLICY_FAILED
    } else {
        EXIT_OK
    };
    if json {
        let mut out = String::new();
        out.push_str("{\"schema_version\":\"1.0\",");
        out.push_str(&format!(
            "\"tool\":{{\"name\":\"envwrap\",\"version\":\"{}\"}},",
            crate::tool_version()
        ));
        out.push_str(&format!("\"ok\":{},", code == EXIT_OK));
        out.push_str("\"findings\":[");
        for (i, f) in report.findings.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            out.push_str(&format!(
                "{{\"id\":\"{}\",\"severity\":\"{}\",\"variable\":\"{}\",\"message\":\"{}\"}}",
                json_escape(f.id),
                severity_name(f.severity),
                json_escape(&f.variable),
                json_escape(&f.message)
            ));
        }
        out.push_str(&format!(
            "],\"counts\":{{\"variables\":{},\"mirrors\":{},\"proxy_checks\":{}}}}}\n",
            report.counts.variables, report.counts.mirrors, report.counts.proxy_checks
        ));
        print!("{out}");
    } else {
        for f in &report.findings {
            eprintln!(
                "envwrap: {}: {}: {}: {}",
                severity_name(f.severity),
                f.id,
                f.variable,
                f.message
            );
        }
        if code == EXIT_OK {
            println!(
                "ok: {} variables, {} mirror pairs, {} proxy checks",
                report.counts.variables, report.counts.mirrors, report.counts.proxy_checks
            );
        }
    }
    code
}

fn severity_name(s: Severity) -> &'static str {
    match s {
        Severity::Error => "error",
        Severity::Warning => "warning",
    }
}
