import React from "react";
import { SecurityAuditReport, VulnerabilityReport, SecretLeak, SecureCodeViolation } from "../api";

interface SecurityDrawerProps {
  isOpen: boolean;
  onClose: () => void;
  report: SecurityAuditReport | null;
  isRunning: boolean;
  onNodeFocus: (nodeId: string) => void;
}

export const SecurityDrawer: React.FC<SecurityDrawerProps> = ({
  isOpen,
  onClose,
  report,
  isRunning,
  onNodeFocus,
}) => {
  if (!isOpen) return null;

  // Compute security score letter grade
  const getLetterGrade = (score: number) => {
    if (score >= 95) return { grade: "A+", color: "grade-emerald", text: "Excellent" };
    if (score >= 90) return { grade: "A", color: "grade-emerald", text: "Secure" };
    if (score >= 80) return { grade: "B", color: "grade-sky", text: "Good" };
    if (score >= 70) return { grade: "C", color: "grade-amber", text: "Moderate" };
    if (score >= 60) return { grade: "D", color: "grade-orange", text: "Warning" };
    return { grade: "F", color: "grade-ruby", text: "Critical Risk" };
  };

  const score = report?.security_score ?? 100;
  const rating = getLetterGrade(score);

  // SVG parameters for radial progress circle
  const radius = 54;
  const circumference = 2 * Math.PI * radius;
  const strokeDashoffset = circumference - (score / 100) * circumference;

  return (
    <div className={`security-drawer-overlay ${isOpen ? "open" : ""}`} onClick={onClose}>
      <div
        className="security-drawer-content"
        onClick={(e) => e.stopPropagation()}
      >
        {/* Drawer Header */}
        <div className="security-drawer-header">
          <div className="security-title-wrap">
            <span className="security-shield-icon">🛡️</span>
            <div>
              <h3>Security Audit Report</h3>
              <p className="security-subtitle">Real-time dependency & code compliance analysis</p>
            </div>
          </div>
          <button className="security-drawer-close" onClick={onClose}>
            &times;
          </button>
        </div>

        {/* Scrollable Content Body */}
        <div className="security-drawer-body">
          {isRunning ? (
            <div className="security-loading-state">
              <div className="security-spinner"></div>
              <p>Analyzing codebase dependencies, scanning credential keys, and auditing OWASP secure coding compliance...</p>
            </div>
          ) : !report ? (
            <div className="security-empty-state">
              <span className="security-empty-shield">🛡️</span>
              <p>No active scan reports found. Click the Audit button to run a complete static analysis security check.</p>
            </div>
          ) : (
            <>
              {/* Score Dashboard Card */}
              <div className="security-score-card glass">
                <div className="security-radial-wrap">
                  <svg className="security-radial-svg" width="130" height="130">
                    <circle
                      className="security-radial-bg"
                      cx="65"
                      cy="65"
                      r={radius}
                      strokeWidth="8"
                    />
                    <circle
                      className={`security-radial-bar ${rating.color}`}
                      cx="65"
                      cy="65"
                      r={radius}
                      strokeWidth="8"
                      strokeDasharray={circumference}
                      strokeDashoffset={strokeDashoffset}
                      strokeLinecap="round"
                    />
                  </svg>
                  <div className="security-score-text">
                    <span className="security-score-num">{score}</span>
                    <span className="security-score-label">/100</span>
                  </div>
                </div>
                <div className="security-grade-info">
                  <span className={`security-grade-badge ${rating.color}`}>
                    Grade {rating.grade}
                  </span>
                  <h4>Crate Health Status: {rating.text}</h4>
                  <p>
                    Found {report.vulnerabilities.filter(v => v.advisory_id !== "OFFLINE").length} CVE dependencies,{" "}
                    {report.leaked_secrets.length} exposed keys, and{" "}
                    {report.secure_code_violations.length} static violations.
                  </p>
                </div>
              </div>

              {/* 1. Exposed Secrets Panel */}
              <div className="security-section">
                <h4 className="security-section-title">
                  🗝️ Credentials & Key Leaks ({report.leaked_secrets.length})
                </h4>
                {report.leaked_secrets.length === 0 ? (
                  <div className="security-issue-empty glass">
                    <span className="emerald-check">✓</span> No hardcoded credential keys or database passwords detected in node configurations.
                  </div>
                ) : (
                  <div className="security-issues-list">
                    {report.leaked_secrets.map((leak, idx) => (
                      <div key={idx} className="security-issue-item glass border-ruby animate-slide-in">
                        <div className="issue-header">
                          <span className="issue-badge badge-ruby">{leak.secret_type}</span>
                          <button
                            className="issue-locate-btn"
                            title="Focus Node on Canvas"
                            onClick={() => onNodeFocus(leak.node_id)}
                          >
                            Locate Node 🔍
                          </button>
                        </div>
                        <p className="issue-msg">{leak.message}</p>
                        <div className="issue-metadata">
                          <span><strong>Field:</strong> <code>{leak.field}</code></span>
                          <span><strong>Value:</strong> <code className="masked-val">{leak.masked_value}</code></span>
                        </div>
                      </div>
                    ))}
                  </div>
                )}
              </div>

              {/* 2. OWASP Code Violations Panel */}
              <div className="security-section">
                <h4 className="security-section-title">
                  ⚠️ Secure Code Compliance ({report.secure_code_violations.length})
                </h4>
                {report.secure_code_violations.length === 0 ? (
                  <div className="security-issue-empty glass">
                    <span className="emerald-check">✓</span> 100% compliant with OWASP secure coding directives. No SQLi or crypto risks found.
                  </div>
                ) : (
                  <div className="security-issues-list">
                    {report.secure_code_violations.map((violation, idx) => (
                      <div
                        key={idx}
                        className={`security-issue-item glass border-orange animate-slide-in`}
                        style={{ animationDelay: `${idx * 0.1}s` }}
                      >
                        <div className="issue-header">
                          <span className="issue-badge badge-orange">{violation.violation_type}</span>
                          <button
                            className="issue-locate-btn"
                            title="Focus Node on Canvas"
                            onClick={() => onNodeFocus(violation.node_id)}
                          >
                            Locate Node 🔍
                          </button>
                        </div>
                        <p className="issue-msg">{violation.message}</p>
                        <div className="issue-advice">
                          <strong>Remediation Advice:</strong> {violation.advice}
                        </div>
                      </div>
                    ))}
                  </div>
                )}
              </div>

              {/* 3. Dependency CVEs Panel */}
              <div className="security-section">
                <h4 className="security-section-title">
                  📦 Dependency Vulnerabilities ({report.vulnerabilities.filter(v => v.advisory_id !== "OFFLINE").length})
                </h4>
                {report.vulnerabilities.length === 0 ? (
                  <div className="security-issue-empty glass">
                    <span className="emerald-check">✓</span> All crate dependencies match clean RUSTSEC audits on OSV.dev.
                  </div>
                ) : report.vulnerabilities.length === 1 && report.vulnerabilities[0].advisory_id === "OFFLINE" ? (
                  <div className="security-issue-offline glass">
                    <span className="offline-warning">⚠️</span> {report.vulnerabilities[0].summary}
                  </div>
                ) : (
                  <div className="security-issues-list">
                    {report.vulnerabilities.map((vuln, idx) => (
                      <div
                        key={idx}
                        className={`security-issue-item glass border-warning animate-slide-in`}
                        style={{ animationDelay: `${idx * 0.05}s` }}
                      >
                        <div className="issue-header">
                          <span className="issue-badge badge-warning">{vuln.advisory_id}</span>
                          <span className={`severity-tag ${vuln.severity.toLowerCase()}`}>
                            {vuln.severity}
                          </span>
                        </div>
                        <p className="issue-crate">
                          Crate: <strong>{vuln.crate_name}</strong> · Version: <code>{vuln.version}</code>
                        </p>
                        <p className="issue-msg">{vuln.summary}</p>
                        <div className="issue-advisory-link">
                          <a href={vuln.url} target="_blank" rel="noopener noreferrer">
                            Read Security Advisory ↗
                          </a>
                        </div>
                      </div>
                    ))}
                  </div>
                )}
              </div>
            </>
          )}
        </div>
      </div>
    </div>
  );
};
