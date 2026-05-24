//! Security Auditing Engine — scans dependency graphs, secret leaks, and OWASP violations.

use std::collections::HashSet;
use std::fs;
use std::path::Path;
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use crate::projects::Graph;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VulnerabilityReport {
    pub crate_name: String,
    pub version: String,
    pub advisory_id: String,
    pub summary: String,
    pub severity: String, // "CRITICAL" | "HIGH" | "MEDIUM" | "LOW"
    pub url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SecretLeak {
    pub node_id: String,
    pub field: String,
    pub secret_type: String,
    pub masked_value: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SecureCodeViolation {
    pub node_id: String,
    pub violation_type: String,
    pub message: String,
    pub severity: String, // "CRITICAL" | "HIGH" | "MEDIUM" | "LOW"
    pub advice: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SecurityAuditReport {
    pub vulnerabilities: Vec<VulnerabilityReport>,
    pub leaked_secrets: Vec<SecretLeak>,
    pub secure_code_violations: Vec<SecureCodeViolation>,
    pub security_score: usize, // 0..=100
}

// ---------------------------------------------------------------------------
// OSV.dev API Models
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct OSVQueryBatch {
    queries: Vec<OSVQuery>,
}

#[derive(Serialize)]
struct OSVQuery {
    package: OSVPackage,
    version: String,
}

#[derive(Serialize)]
struct OSVPackage {
    name: String,
    ecosystem: &'static str, // "crates.io"
}

#[derive(Deserialize)]
struct OSVBatchResponse {
    results: Option<Vec<OSVResult>>,
}

#[derive(Deserialize)]
struct OSVResult {
    vulns: Option<Vec<OSVVulnMinimal>>,
}

#[derive(Deserialize)]
struct OSVVulnMinimal {
    id: String,
}

#[derive(Deserialize)]
#[allow(dead_code)]
struct OSVVulnDetails {
    id: String,
    summary: Option<String>,
    details: Option<String>,
    references: Option<Vec<OSVReference>>,
    affected: Option<Vec<OSVAffected>>,
}

#[derive(Deserialize, Clone)]
struct OSVReference {
    #[serde(rename = "type")]
    ref_type: String,
    url: String,
}

#[derive(Deserialize)]
#[allow(dead_code)]
struct OSVAffected {
    package: Option<OSVAffectedPackage>,
    ecosystem_specific: Option<serde_json::Value>,
    database_specific: Option<serde_json::Value>,
}

#[derive(Deserialize)]
#[allow(dead_code)]
struct OSVAffectedPackage {
    name: String,
}

// ---------------------------------------------------------------------------
// Implementation
// ---------------------------------------------------------------------------

/// Parse `Cargo.lock` and extract a flat list of dependency name & exact version pairs.
pub fn parse_cargo_lock_dependencies(project_dir: &Path) -> Result<Vec<(String, String)>, anyhow::Error> {
    let lock_path = project_dir.join("Cargo.lock");
    if !lock_path.exists() {
        info!("Cargo.lock not found in user project dir: {:?}", project_dir);
        return Ok(Vec::new());
    }

    let contents = fs::read_to_string(&lock_path)?;
    let doc = contents.parse::<toml_edit::DocumentMut>()?;
    let mut dependencies = Vec::new();

    if let Some(pkg_val) = doc.get("package") {
        if let Some(array_of_tables) = pkg_val.as_array_of_tables() {
            for table in array_of_tables {
                let name = table.get("name").and_then(|v| v.as_str());
                let version = table.get("version").and_then(|v| v.as_str());
                if let (Some(n), Some(v)) = (name, version) {
                    dependencies.push((n.to_string(), v.to_string()));
                }
            }
        }
    }

    Ok(dependencies)
}

/// Query OSV.dev batch endpoint to identify crate vulnerabilities, then fetch their full advisory details.
pub async fn check_dependency_cves(dependencies: &[(String, String)]) -> Result<Vec<VulnerabilityReport>, anyhow::Error> {
    if dependencies.is_empty() {
        return Ok(Vec::new());
    }

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()?;

    // 1. Build query list
    let queries: Vec<OSVQuery> = dependencies
        .iter()
        .map(|(name, version)| OSVQuery {
            package: OSVPackage {
                name: name.clone(),
                ecosystem: "crates.io",
            },
            version: version.clone(),
        })
        .collect();

    let batch_req = OSVQueryBatch { queries };

    // 2. Query batch API
    let response = client
        .post("https://api.osv.dev/v1/querybatch")
        .json(&batch_req)
        .send()
        .await?;

    if !response.status().is_success() {
        return Err(anyhow::anyhow!("OSV.dev querybatch returned HTTP {}", response.status()));
    }

    let batch_res = response.json::<OSVBatchResponse>().await?;
    let mut vuln_ids = HashSet::new();
    let mut crate_by_vuln = std::collections::HashMap::new();

    if let Some(results) = batch_res.results {
        for (i, result) in results.iter().enumerate() {
            if let Some(ref vulns) = result.vulns {
                let (crate_name, version) = &dependencies[i];
                for vuln in vulns {
                    vuln_ids.insert(vuln.id.clone());
                    // Keep track of which crate triggered this vuln
                    crate_by_vuln.insert(vuln.id.clone(), (crate_name.clone(), version.clone()));
                }
            }
        }
    }

    if vuln_ids.is_empty() {
        return Ok(Vec::new());
    }

    // 3. Sequentially fetch details for each distinct vulnerability
    let mut reports = Vec::new();
 
    for id in vuln_ids {
        let url = format!("https://api.osv.dev/v1/vulns/{}", id);
        let res = client.get(&url).send().await;
        if let Ok(resp) = res {
            if resp.status().is_success() {
                if let Ok(details) = resp.json::<OSVVulnDetails>().await {
                    let (crate_name, version) = crate_by_vuln
                        .get(&details.id)
                        .cloned()
                        .unwrap_or_else(|| ("unknown".to_string(), "unknown".to_string()));
 
                    // Extract severity
                    let severity = details
                        .affected
                        .as_ref()
                        .and_then(|affs| {
                            affs.iter().find_map(|aff| {
                                aff.ecosystem_specific
                                    .as_ref()
                                    .and_then(|v| v.get("severity").and_then(|s| s.as_str().map(|s| s.to_uppercase())))
                                    .or_else(|| {
                                        aff.database_specific
                                            .as_ref()
                                            .and_then(|v| v.get("severity").and_then(|s| s.as_str().map(|s| s.to_uppercase())))
                                    })
                            })
                        })
                        .unwrap_or_else(|| "MEDIUM".to_string());
 
                    // Extract URL references
                    let url = details
                        .references
                        .as_ref()
                        .and_then(|refs| {
                            refs.iter()
                                .find(|r| r.ref_type == "ADVISORY" || r.ref_type == "WEB")
                                .map(|r| r.url.clone())
                                .or_else(|| refs.first().map(|r| r.url.clone()))
                        })
                        .unwrap_or_else(|| format!("https://rustsec.org/advisories/{}.html", details.id));
 
                    reports.push(VulnerabilityReport {
                        crate_name,
                        version,
                        advisory_id: details.id,
                        summary: details.summary.unwrap_or_else(|| "No summary available".to_string()),
                        severity,
                        url,
                    });
                }
            }
        }
    }

    Ok(reports)
}

/// Regex-based API Key and Secret Leak scanner.
pub fn check_leaked_secrets(graph: &Graph) -> Vec<SecretLeak> {
    let mut leaks = Vec::new();

    // Regex compile patterns
    let aws_key_re = regex::Regex::new(r"AKIA[0-9A-Z]{16}").unwrap();
    let aws_secret_re = regex::Regex::new(r#"(?i)aws(.{0,20})?['"][0-9a-zA-Z/+]{40}['"]"#).unwrap();
    let slack_re = regex::Regex::new(r"https://hooks\.slack\.com/services/T[A-Z0-9]{8}/B[A-Z0-9]{8}/[A-Za-z0-9]{24}").unwrap();
    let priv_key_re = regex::Regex::new(r"(?s)-----BEGIN\s+([A-Z0-9\s_]+)\s+PRIVATE\s+KEY-----").unwrap();
    let generic_re = regex::Regex::new(r#"(?i)(api_key|token|secret|password|passwd|auth_token)\s*=\s*['"][^'"]{8,}['"]"#).unwrap();
    let db_pass_re = regex::Regex::new(r"(postgres|mysql|sqlite|mongodb)://[^:]+:([^@]+)@").unwrap();


    for node in &graph.nodes {
        let node_id = node.id.0.clone();

        if let Some(obj) = node.config.as_object() {
            for (field, val) in obj {
                if let Some(val_str) = val.as_str() {
                    // 1. AWS Key
                    if aws_key_re.is_match(val_str) {
                        leaks.push(SecretLeak {
                            node_id: node_id.clone(),
                            field: field.clone(),
                            secret_type: "AWS Access Key ID".to_string(),
                            masked_value: "AKIA****************".to_string(),
                            message: "AWS Access Key ID leaked inside visual node config.".to_string(),
                        });
                    }

                    // 2. AWS Secret
                    if aws_secret_re.is_match(val_str) {
                        leaks.push(SecretLeak {
                            node_id: node_id.clone(),
                            field: field.clone(),
                            secret_type: "AWS Secret Access Key".to_string(),
                            masked_value: "******** (AWS Secret Access Key)".to_string(),
                            message: "AWS Secret Access Key leaked inside visual node config.".to_string(),
                        });
                    }

                    // 3. Slack Webhook
                    if slack_re.is_match(val_str) {
                        leaks.push(SecretLeak {
                            node_id: node_id.clone(),
                            field: field.clone(),
                            secret_type: "Slack Webhook URL".to_string(),
                            masked_value: "https://hooks.slack.com/services/T********/B********/********".to_string(),
                            message: "Slack Webhook URL leaked inside node configuration parameters.".to_string(),
                        });
                    }

                    // 4. Private Keys
                    if priv_key_re.is_match(val_str) {
                        leaks.push(SecretLeak {
                            node_id: node_id.clone(),
                            field: field.clone(),
                            secret_type: "RSA/ECC Private Key".to_string(),
                            masked_value: "-----BEGIN PRIVATE KEY----- ...".to_string(),
                            message: "RSA/ECC Private Key block hardcoded in node parameters.".to_string(),
                        });
                    }

                    // 5. Generic API token / Secret assignment
                    if let Some(cap) = generic_re.captures(val_str) {
                        let sec_param = cap.get(1).map(|m| m.as_str()).unwrap_or("secret");
                        leaks.push(SecretLeak {
                            node_id: node_id.clone(),
                            field: field.clone(),
                            secret_type: "Hardcoded API Key/Token".to_string(),
                            masked_value: "******** (API Token)".to_string(),
                            message: format!("Potential credential leak: variable '{}' assigned a hardcoded secret.", sec_param),
                        });
                    }

                    // 6. DB connection password
                    if let Some(cap) = db_pass_re.captures(val_str) {
                        let db_type = cap.get(1).map(|m| m.as_str()).unwrap_or("db");
                        let raw_pwd = cap.get(2).map(|m| m.as_str()).unwrap_or("pwd");
                        let masked_pwd = "*".repeat(raw_pwd.len());
                        leaks.push(SecretLeak {
                            node_id: node_id.clone(),
                            field: field.clone(),
                            secret_type: "Database Password Leak".to_string(),
                            masked_value: format!("{}://root:{}@host/db", db_type, masked_pwd),
                            message: "Database password exposed in plain text inside connection string.".to_string(),
                        });
                    }
                }
            }
        }
    }

    leaks
}

/// OWASP secure coding static analysis rules scanner.
pub fn check_owasp_violations(graph: &Graph) -> Vec<SecureCodeViolation> {
    let mut violations = Vec::new();

    for node in &graph.nodes {
        let node_id = node.id.0.clone();
        let template_id = node.template_id.as_str();

        // 1. Scan custom.block implementations
        if template_id == "custom.block" {
            if let Some(code_val) = node.config.get("code").and_then(|v| v.as_str()) {
                // A. Check SQL Injection (A03:2021-Injection)
                let performs_sql = code_val.contains(".execute(") 
                    || code_val.contains(".query(") 
                    || code_val.contains(".query_row(")
                    || code_val.contains("rusqlite::")
                    || code_val.contains("tokio_rusqlite");

                let uses_format_in_sql = code_val.contains("format!") 
                    || code_val.contains(".push_str(") 
                    || code_val.contains(" + ");

                if performs_sql && uses_format_in_sql {
                    violations.push(SecureCodeViolation {
                        node_id: node_id.clone(),
                        violation_type: "SQL Injection Risk (OWASP A03)".to_string(),
                        message: "Static analysis detected dynamic string formatting (e.g. 'format!') within a code block performing SQL queries.".to_string(),
                        severity: "CRITICAL".to_string(),
                        advice: "Always use parameterized queries with placeholder bindings (e.g. '?' or '$1') rather than manual string concatenation to completely neutralize SQL injection.".to_string(),
                    });
                }

                // B. Check Cryptographic Failures (A02:2021-Cryptographic Failures)
                if code_val.contains("md5::") || code_val.contains("sha1::") || code_val.contains("md5") || code_val.contains("sha1") {
                    violations.push(SecureCodeViolation {
                        node_id: node_id.clone(),
                        violation_type: "Weak Hashing Algorithm (OWASP A02)".to_string(),
                        message: "Usage of MD5 or SHA-1 detected. These algorithms are cryptographically broken and vulnerable to collision attacks.".to_string(),
                        severity: "HIGH".to_string(),
                        advice: "Upgrade cryptographic operations to modern secure standards like bcrypt, Argon2, or SHA-256/SHA-512.".to_string(),
                    });
                }

                // C. SSRF Vulnerability (OWASP A10)
                if (code_val.contains("reqwest::") || code_val.contains("reqwest::Client")) && (code_val.contains("payload") || code_val.contains("input")) && !code_val.contains(".contains(") {
                    violations.push(SecureCodeViolation {
                        node_id: node_id.clone(),
                        violation_type: "Server-Side Request Forgery Risk (OWASP A10)".to_string(),
                        message: "Outbound HTTP requests are driven dynamically by dynamic payloads without validation.".to_string(),
                        severity: "HIGH".to_string(),
                        advice: "Validate or whitelist target URLs before triggering outbound requests, preventing attackers from mapping internal network services.".to_string(),
                    });
                }
            }
        }

        // 2. Scan Axum handler middlewares (A01:2021-Broken Access Control)
        if template_id == "http.handler" {
            if let Some(path_val) = node.config.get("path").and_then(|v| v.as_str()) {
                let is_sensitive = path_val.contains("/admin") 
                    || path_val.contains("/delete") 
                    || path_val.contains("/update")
                    || path_val.contains("/secrets");

                // Check if CORS is open or authorization is completely absent in graph configuration wires
                // We'll inspect if there is an auth state or custom authorization block configured
                if is_sensitive {
                    let has_auth = node.config.get("require_auth").and_then(|v| v.as_bool()).unwrap_or(false)
                        || path_val.contains("/public"); // exempt explicit public overrides
                    
                    if !has_auth {
                        violations.push(SecureCodeViolation {
                            node_id: node_id.clone(),
                            violation_type: "Broken Access Control (OWASP A01)".to_string(),
                            message: format!("Sensitive endpoint path '{}' is exposed without explicit require_auth protection.", path_val),
                            severity: "HIGH".to_string(),
                            advice: "Activate require_auth token checks in handler configuration properties to restrict access to authorized calls.".to_string(),
                        });
                    }
                }
            }
        }
    }

    violations
}

/// Executes the entire Security Audit scan suite and calculates a weighted Security Score.
pub fn calculate_security_score(
    vulns: &[VulnerabilityReport],
    secrets: &[SecretLeak],
    violations: &[SecureCodeViolation],
) -> usize {
    let mut base_score: isize = 100;

    // 1. Dependency CVE deductions (capped at 40)
    let mut cve_deduction = 0;
    for vuln in vulns {
        // Skip OFFLINE indicator deduction
        if vuln.advisory_id == "OFFLINE" {
            continue;
        }
        match vuln.severity.as_str() {
            "CRITICAL" => cve_deduction += 10,
            "HIGH" => cve_deduction += 8,
            "MEDIUM" => cve_deduction += 5,
            "LOW" => cve_deduction += 2,
            _ => cve_deduction += 5, // moderate/unknown
        }
    }
    base_score -= cve_deduction.min(40);

    // 2. Secret leak deductions (capped at 30)
    let secret_deduction = (secrets.len() * 15) as isize;
    base_score -= secret_deduction.min(30);

    // 3. OWASP violations deductions (capped at 30)
    let mut owasp_deduction = 0;
    for violation in violations {
        match violation.severity.as_str() {
            "CRITICAL" => owasp_deduction += 10,
            "HIGH" => owasp_deduction += 8,
            "MEDIUM" => owasp_deduction += 5,
            _ => owasp_deduction += 5,
        }
    }
    base_score -= owasp_deduction.min(30);

    base_score.max(0) as usize
}

/// Run full visual studio project security auditing.
pub async fn run_security_audit(project_dir: &Path, graph: &Graph) -> SecurityAuditReport {
    // 1. Dependency parsing
    let dependencies = match parse_cargo_lock_dependencies(project_dir) {
        Ok(deps) => deps,
        Err(e) => {
            warn!("Failed to parse Cargo.lock: {}", e);
            Vec::new()
        }
    };

    // 2. CVE scans
    let vulnerabilities = match check_dependency_cves(&dependencies).await {
        Ok(v) => v,
        Err(e) => {
            warn!("OSV.dev dependency audit failed: {}", e);
            vec![VulnerabilityReport {
                crate_name: "Cargo.lock Audit".to_string(),
                version: "N/A".to_string(),
                advisory_id: "OFFLINE".to_string(),
                summary: "OSV.dev vulnerability lookup was unreachable. Dependency security scans are currently offline.".to_string(),
                severity: "MEDIUM".to_string(),
                url: "https://osv.dev/".to_string(),
            }]
        }
    };

    // 3. Secrets leaks checks
    let leaked_secrets = check_leaked_secrets(graph);

    // 4. Secure coding violations
    let secure_code_violations = check_owasp_violations(graph);

    // 5. Calculate final score
    let security_score = calculate_security_score(&vulnerabilities, &leaked_secrets, &secure_code_violations);

    SecurityAuditReport {
        vulnerabilities,
        leaked_secrets,
        secure_code_violations,
        security_score,
    }
}

// ---------------------------------------------------------------------------
// Unit Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::projects::types::{Node, NodeId};
    use crate::templates::TemplateId;
    use serde_json::json;

    #[test]
    fn test_cargo_lock_dependencies_parsing() {
        let temp = tempfile::tempdir().unwrap();
        let project_dir = temp.path();

        // Write a mock Cargo.lock TOML file
        let lock_content = r#"
[[package]]
name = "serde"
version = "1.0.197"
source = "registry+https://github.com/rust-lang/crates.io-index"
checksum = "3fb1c04d38b1a55db1192f0f2cfd11a0de24cc1858a74e59f2ed5215d2a90e5f"

[[package]]
name = "tokio"
version = "1.35.1"
dependencies = [
 "pin-project-lite",
]
"#;
        fs::write(project_dir.join("Cargo.lock"), lock_content).unwrap();

        let deps = parse_cargo_lock_dependencies(project_dir).unwrap();
        assert_eq!(deps.len(), 2);
        assert_eq!(deps[0], ("serde".to_string(), "1.0.197".to_string()));
        assert_eq!(deps[1], ("tokio".to_string(), "1.35.1".to_string()));
    }

    #[test]
    fn test_secret_scanner_identifies_aws_keys_and_tokens() {
        let graph = Graph {
            schema_version: 1,
            nodes: vec![
                Node {
                    id: NodeId("node_1".to_string()),
                    template_id: TemplateId::new("language.fn").unwrap(),
                    position: crate::projects::types::Position { x: 0.0, y: 0.0 },
                    config: json!({
                        "name": "checkAws",
                        "aws_key": "AKIA1234567890ABCDEF",
                        "aws_secret": "my aws secret: 'abc123DEF456/789xyz123ABCdef456XYZ/789+A'"
                    }),
                    label: None,
                    comment: None,
                },
                Node {
                    id: NodeId("node_2".to_string()),
                    template_id: TemplateId::new("integration.scheduler").unwrap(),
                    position: crate::projects::types::Position { x: 0.0, y: 0.0 },
                    config: json!({
                        "slack_url": "https://hooks.slack.com/services/T12345678/B12345678/A123456789012345678901234"
                    }),
                    label: None,
                    comment: None,
                },
                Node {
                    id: NodeId("node_3".to_string()),
                    template_id: TemplateId::new("custom.block").unwrap(),
                    position: crate::projects::types::Position { x: 0.0, y: 0.0 },
                    config: json!({
                        "name": "sql_exec",
                        "db_conn": "postgres://root:my_secret_pwd@localhost/database"
                    }),
                    label: None,
                    comment: None,
                }
            ],
            edges: vec![],
        };

        let leaks = check_leaked_secrets(&graph);
        assert_eq!(leaks.len(), 4);

        // Verify Slack
        let slack_leak = leaks.iter().find(|l| l.secret_type == "Slack Webhook URL").unwrap();
        assert_eq!(slack_leak.node_id, "node_2");
        assert_eq!(slack_leak.masked_value, "https://hooks.slack.com/services/T********/B********/********");

        // Verify AWS Access Key ID
        let aws_key_leak = leaks.iter().find(|l| l.secret_type == "AWS Access Key ID").unwrap();
        assert_eq!(aws_key_leak.node_id, "node_1");

        // Verify DB password
        let db_leak = leaks.iter().find(|l| l.secret_type == "Database Password Leak").unwrap();
        assert_eq!(db_leak.node_id, "node_3");
        assert_eq!(db_leak.masked_value, "postgres://root:*************@host/db");
    }

    #[test]
    fn test_owasp_sql_injection_violation_check() {
        let graph = Graph {
            schema_version: 1,
            nodes: vec![
                Node {
                    id: NodeId("node_1".to_string()),
                    template_id: TemplateId::new("custom.block").unwrap(),
                    position: crate::projects::types::Position { x: 0.0, y: 0.0 },
                    config: json!({
                        "code": r#"
                            let sql = format!("SELECT * FROM users WHERE name = '{}'", input.name);
                            conn.execute(&sql, ()).await?;
                        "#
                    }),
                    label: None,
                    comment: None,
                },
                Node {
                    id: NodeId("node_2".to_string()),
                    template_id: TemplateId::new("custom.block").unwrap(),
                    position: crate::projects::types::Position { x: 0.0, y: 0.0 },
                    config: json!({
                        "code": r#"
                            // MD5 crypto failure
                            let hash = md5::compute(password);
                        "#
                    }),
                    label: None,
                    comment: None,
                },
                Node {
                    id: NodeId("node_3".to_string()),
                    template_id: TemplateId::new("http.handler").unwrap(),
                    position: crate::projects::types::Position { x: 0.0, y: 0.0 },
                    config: json!({
                        "path": "/api/admin/secrets",
                        "require_auth": false
                    }),
                    label: None,
                    comment: None,
                }
            ],
            edges: vec![],
        };

        let violations = check_owasp_violations(&graph);
        assert_eq!(violations.len(), 3);

        // SQL Injection check
        let sqli = violations.iter().find(|v| v.violation_type.contains("SQL Injection")).unwrap();
        assert_eq!(sqli.node_id, "node_1");
        assert_eq!(sqli.severity, "CRITICAL");

        // Hashing check
        let hash_violation = violations.iter().find(|v| v.violation_type.contains("Weak Hashing")).unwrap();
        assert_eq!(hash_violation.node_id, "node_2");
        assert_eq!(hash_violation.severity, "HIGH");

        // Broken access control check
        let auth_violation = violations.iter().find(|v| v.violation_type.contains("Broken Access Control")).unwrap();
        assert_eq!(auth_violation.node_id, "node_3");
    }

    #[test]
    fn test_security_score_weight_calculation() {
        let empty_cves = vec![];
        let empty_leaks = vec![];
        let empty_violations = vec![];
        assert_eq!(calculate_security_score(&empty_cves, &empty_leaks, &empty_violations), 100);

        // Mock vulnerabilities
        let cves = vec![
            VulnerabilityReport {
                crate_name: "serde".to_string(),
                version: "1.0.0".to_string(),
                advisory_id: "RUSTSEC-1".to_string(),
                summary: "CVE 1".to_string(),
                severity: "CRITICAL".to_string(),
                url: "url".to_string(),
            },
            VulnerabilityReport {
                crate_name: "serde".to_string(),
                version: "1.0.0".to_string(),
                advisory_id: "RUSTSEC-2".to_string(),
                summary: "CVE 2".to_string(),
                severity: "HIGH".to_string(),
                url: "url".to_string(),
            },
        ];

        // Deducts: 10 + 8 = 18
        assert_eq!(calculate_security_score(&cves, &empty_leaks, &empty_violations), 82);

        // Secrets
        let leaks = vec![
            SecretLeak {
                node_id: "n1".to_string(),
                field: "f1".to_string(),
                secret_type: "AWS".to_string(),
                masked_value: "masked".to_string(),
                message: "leak".to_string(),
            }
        ];

        // Deducts: 18 + 15 = 33
        assert_eq!(calculate_security_score(&cves, &leaks, &empty_violations), 67);

        // Violations
        let violations = vec![
            SecureCodeViolation {
                node_id: "n2".to_string(),
                violation_type: "SQLI".to_string(),
                message: "msg".to_string(),
                severity: "CRITICAL".to_string(),
                advice: "adv".to_string(),
            }
        ];

        // Deducts: 18 + 15 + 10 = 43
        assert_eq!(calculate_security_score(&cves, &leaks, &violations), 57);
    }
}

