use clap::Parser;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::exit;

#[derive(Parser, Debug)]
#[command(author, version, about = "Generate HTML report from test results")]
struct Args {
    /// Input directory containing JSON test results
    #[arg(short, long)]
    input_dir: PathBuf,

    /// Output HTML file
    #[arg(short, long)]
    output_file: PathBuf,
}

struct TestResult {
    name: String,
    backend: String,
    status: String,
    message: Option<String>,
    duration_ms: u64,
    artifacts: Vec<String>,
}

struct TestReport {
    results: Vec<TestResult>,
    platform: String,
}

fn main() {
    let args = Args::parse();

    // Check if input directory exists
    if !args.input_dir.exists() {
        eprintln!(
            "Input directory does not exist: {}",
            args.input_dir.display()
        );
        exit(1);
    }

    // Get all JSON files in the input directory
    let mut json_files = Vec::new();
    if let Ok(entries) = fs::read_dir(&args.input_dir) {
        for entry in entries.filter_map(Result::ok) {
            let path = entry.path();
            if path.is_file() && path.extension().is_some_and(|ext| ext == "json") {
                json_files.push(path);
            }
        }
    }

    if json_files.is_empty() {
        eprintln!(
            "No JSON test result files found in {}",
            args.input_dir.display()
        );
        exit(1);
    }

    println!("Found {} test result files", json_files.len());

    // Load and parse all test reports
    let mut all_results = Vec::new();
    let mut platform = String::new();

    for file in &json_files {
        if let Ok(content) = fs::read_to_string(file) {
            if let Ok(json) = serde_json::from_str::<serde_json::Value>(&content) {
                if let Some(json_results) = json.get("results").and_then(|v| v.as_array()) {
                    for result in json_results {
                        if let (Some(name), Some(backend), Some(status), Some(duration_ms)) = (
                            result.get("name").and_then(|v| v.as_str()),
                            result.get("backend").and_then(|v| v.as_str()),
                            result.get("status").and_then(|v| v.as_str()),
                            result.get("duration_ms").and_then(|v| v.as_u64()),
                        ) {
                            let message = result
                                .get("message")
                                .and_then(|v| v.as_str())
                                .map(String::from);

                            let artifacts = result
                                .get("artifacts")
                                .and_then(|v| v.as_array())
                                .map(|arr| {
                                    arr.iter()
                                        .filter_map(|a| a.as_str().map(String::from))
                                        .collect()
                                })
                                .unwrap_or_default();

                            all_results.push(TestResult {
                                name: name.to_string(),
                                backend: backend.to_string(),
                                status: status.to_string(),
                                message,
                                duration_ms,
                                artifacts,
                            });
                        }
                    }
                }

                if platform.is_empty() {
                    if let Some(platform_value) = json.get("platform").and_then(|v| v.as_str()) {
                        platform = platform_value.to_string();
                    }
                }
            }
        }
    }

    if all_results.is_empty() {
        eprintln!("No valid test results found in the JSON files");
        exit(1);
    }

    // Generate HTML report
    let html = generate_html_report(&TestReport {
        results: all_results,
        platform,
    });

    // Write HTML report to output file
    if let Some(parent) = args.output_file.parent() {
        if !parent.exists() {
            fs::create_dir_all(parent).unwrap_or_else(|e| {
                eprintln!("Failed to create output directory: {}", e);
                exit(1);
            });
        }
    }

    fs::write(&args.output_file, html).unwrap_or_else(|e| {
        eprintln!("Failed to write output file: {}", e);
        exit(1);
    });

    println!("Test report generated: {}", args.output_file.display());
}

fn generate_html_report(report: &TestReport) -> String {
    // Count results by status
    let mut passed = 0;
    let mut failed = 0;
    let mut skipped = 0;

    for result in &report.results {
        match result.status.as_str() {
            "PASSED" => passed += 1,
            "FAILED" => failed += 1,
            "SKIPPED" => skipped += 1,
            _ => {}
        }
    }

    // Group results by backend
    let mut results_by_backend: HashMap<String, Vec<&TestResult>> = HashMap::new();
    for result in &report.results {
        results_by_backend
            .entry(result.backend.clone())
            .or_default()
            .push(result);
    }

    // Generate HTML
    let mut html = String::new();
    html.push_str("<!DOCTYPE html>");
    html.push_str("<html lang=\"en\">");
    html.push_str("<head>");
    html.push_str("<meta charset=\"UTF-8\">");
    html.push_str("<meta name=\"viewport\" content=\"width=device-width, initial-scale=1.0\">");
    html.push_str("<title>Audio Capture Test Report</title>");
    html.push_str("<style>");
    html.push_str(
        "body { font-family: Arial, sans-serif; margin: 0; padding: 20px; color: #333; }",
    );
    html.push_str("h1, h2, h3 { color: #444; }");
    html.push_str(".summary { display: flex; margin-bottom: 20px; }");
    html.push_str(".summary-box { flex: 1; margin: 10px; padding: 15px; border-radius: 5px; color: white; text-align: center; }");
    html.push_str(".summary-box h2 { color: white; margin: 0; }");
    html.push_str(".passed { background-color: #4CAF50; }");
    html.push_str(".failed { background-color: #F44336; }");
    html.push_str(".skipped { background-color: #FF9800; }");
    html.push_str("table { width: 100%; border-collapse: collapse; margin-bottom: 20px; }");
    html.push_str("th, td { text-align: left; padding: 8px; border-bottom: 1px solid #ddd; }");
    html.push_str("th { background-color: #f2f2f2; }");
    html.push_str("tr:nth-child(even) { background-color: #f9f9f9; }");
    html.push_str(".result-passed { color: #4CAF50; }");
    html.push_str(".result-failed { color: #F44336; }");
    html.push_str(".result-skipped { color: #FF9800; }");
    html.push_str(".artifact { margin: 5px 0; }");
    html.push_str(".artifact a { color: #2196F3; text-decoration: none; }");
    html.push_str(".artifact a:hover { text-decoration: underline; }");
    html.push_str("</style>");
    html.push_str("</head>");
    html.push_str("<body>");

    html.push_str(&format!(
        "<h1>Audio Capture Test Report - Platform: {}</h1>",
        report.platform
    ));

    // Summary section
    html.push_str("<div class=\"summary\">");
    html.push_str(&format!(
        "<div class=\"summary-box passed\"><h2>{}</h2>Passed</div>",
        passed
    ));
    html.push_str(&format!(
        "<div class=\"summary-box failed\"><h2>{}</h2>Failed</div>",
        failed
    ));
    html.push_str(&format!(
        "<div class=\"summary-box skipped\"><h2>{}</h2>Skipped</div>",
        skipped
    ));
    html.push_str("</div>");

    // Results by backend
    for (backend, results) in &results_by_backend {
        html.push_str(&format!("<h2>Backend: {}</h2>", backend));
        html.push_str("<table>");
        html.push_str("<tr><th>Test</th><th>Status</th><th>Duration</th><th>Artifacts</th><th>Message</th></tr>");

        for result in results {
            let status_class = match result.status.as_str() {
                "PASSED" => "result-passed",
                "FAILED" => "result-failed",
                "SKIPPED" => "result-skipped",
                _ => "",
            };

            html.push_str("<tr>");
            html.push_str(&format!("<td>{}</td>", result.name));
            html.push_str(&format!(
                "<td class=\"{}\"><strong>{}</strong></td>",
                status_class, result.status
            ));
            html.push_str(&format!("<td>{}ms</td>", result.duration_ms));

            // Artifacts
            html.push_str("<td>");
            for artifact in &result.artifacts {
                html.push_str(&format!(
                    "<div class=\"artifact\">• <a href=\"{}\">{}</a></div>",
                    artifact,
                    Path::new(artifact)
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                ));
            }
            html.push_str("</td>");

            // Message (if any)
            html.push_str("<td>");
            if let Some(message) = &result.message {
                html.push_str(message);
            }
            html.push_str("</td>");

            html.push_str("</tr>");
        }

        html.push_str("</table>");
    }

    html.push_str("</body>");
    html.push_str("</html>");

    html
}
