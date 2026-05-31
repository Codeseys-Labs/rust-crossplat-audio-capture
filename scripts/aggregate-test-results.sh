#!/bin/bash

# Test result aggregation script
# Collects and aggregates test results from all platform containers

set -euo pipefail

# Configuration
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
RESULTS_DIR="${PROJECT_ROOT}/test-results"
TIMESTAMP=$(date +"%Y%m%d_%H%M%S")

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

# Logging functions
log_info() {
    echo -e "${BLUE}[INFO]${NC} $1"
}

log_success() {
    echo -e "${GREEN}[SUCCESS]${NC} $1"
}

log_warning() {
    echo -e "${YELLOW}[WARNING]${NC} $1"
}

log_error() {
    echo -e "${RED}[ERROR]${NC} $1"
}

# Collect results from a platform directory
collect_platform_results() {
    local platform="$1"
    local platform_dir="$RESULTS_DIR/$platform"
    
    if [ ! -d "$platform_dir" ]; then
        log_warning "No results directory found for platform: $platform"
        echo '{"status": "no_results", "platform": "'$platform'"}'
        return
    fi
    
    # Find the most recent results
    local latest_summary=$(find "$platform_dir" -name "summary_*.json" -type f | sort | tail -1)
    local latest_compilation=$(find "$platform_dir" -name "compilation_*.json" -type f | sort | tail -1)
    
    local result='{"platform": "'$platform'", "status": "unknown"}'
    
    if [ -n "$latest_summary" ] && [ -f "$latest_summary" ]; then
        result=$(cat "$latest_summary")
        log_info "Found summary for $platform: $latest_summary"
    elif [ -n "$latest_compilation" ] && [ -f "$latest_compilation" ]; then
        result=$(cat "$latest_compilation")
        result=$(echo "$result" | jq '. + {"platform": "'$platform'"}')
        log_info "Found compilation results for $platform: $latest_compilation"
    else
        log_warning "No result files found for platform: $platform"
    fi
    
    echo "$result"
}

# Generate aggregated JSON report
generate_json_report() {
    local output_file="$1"
    
    log_info "Generating aggregated JSON report..."
    
    # Collect results from all platforms
    local linux_results=$(collect_platform_results "linux")
    local windows_results=$(collect_platform_results "windows")
    local macos_results=$(collect_platform_results "macos")
    
    # Create aggregated report
    cat > "$output_file" << EOF
{
    "report_metadata": {
        "generated_at": "${TIMESTAMP}",
        "generator": "aggregate-test-results.sh",
        "version": "1.0"
    },
    "platforms": {
        "linux": $linux_results,
        "windows": $windows_results,
        "macos": $macos_results
    },
    "summary": {
        "total_platforms": 3,
        "successful_platforms": 0,
        "failed_platforms": 0,
        "overall_status": "unknown"
    }
}
EOF
    
    # Calculate summary statistics
    local successful=0
    local failed=0
    
    for platform in "linux" "windows" "macos"; do
        local platform_status=$(jq -r ".platforms.$platform.compilation // .platforms.$platform.status // \"unknown\"" "$output_file")
        case "$platform_status" in
            "success")
                ((successful++))
                ;;
            "failed"|"error")
                ((failed++))
                ;;
        esac
    done
    
    local overall_status="unknown"
    if [ $successful -eq 3 ]; then
        overall_status="all_success"
    elif [ $failed -eq 0 ]; then
        overall_status="partial_success"
    else
        overall_status="has_failures"
    fi
    
    # Update summary
    jq --arg successful "$successful" --arg failed "$failed" --arg overall "$overall_status" '
        .summary.successful_platforms = ($successful | tonumber) |
        .summary.failed_platforms = ($failed | tonumber) |
        .summary.overall_status = $overall
    ' "$output_file" > "${output_file}.tmp" && mv "${output_file}.tmp" "$output_file"
    
    log_success "JSON report generated: $output_file"
}

# Generate HTML dashboard
generate_html_dashboard() {
    local json_file="$1"
    local output_file="$2"
    
    log_info "Generating HTML dashboard..."
    
    if [ ! -f "$json_file" ]; then
        log_error "JSON report file not found: $json_file"
        return 1
    fi
    
    # Extract data from JSON
    local timestamp=$(jq -r '.report_metadata.generated_at' "$json_file")
    local overall_status=$(jq -r '.summary.overall_status' "$json_file")
    local successful_count=$(jq -r '.summary.successful_platforms' "$json_file")
    local failed_count=$(jq -r '.summary.failed_platforms' "$json_file")
    
    # Determine status colors and icons
    local status_class="warning"
    local status_icon="⚠️"
    local status_text="Unknown"
    
    case "$overall_status" in
        "all_success")
            status_class="success"
            status_icon="✅"
            status_text="All Platforms Successful"
            ;;
        "partial_success")
            status_class="warning"
            status_icon="⚠️"
            status_text="Partial Success"
            ;;
        "has_failures")
            status_class="danger"
            status_icon="❌"
            status_text="Some Platforms Failed"
            ;;
    esac
    
    # Create HTML dashboard
    cat > "$output_file" << EOF
<!DOCTYPE html>
<html lang="en">
<head>
    <meta charset="UTF-8">
    <meta name="viewport" content="width=device-width, initial-scale=1.0">
    <title>Cross-Platform Audio Capture - Test Dashboard</title>
    <style>
        * { margin: 0; padding: 0; box-sizing: border-box; }
        body { font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, sans-serif; background-color: #f8f9fa; }
        .container { max-width: 1200px; margin: 0 auto; padding: 20px; }
        .header { text-align: center; margin-bottom: 40px; }
        .header h1 { color: #2c3e50; margin-bottom: 10px; }
        .header .subtitle { color: #7f8c8d; font-size: 18px; }
        .status-banner { padding: 20px; border-radius: 8px; margin-bottom: 30px; text-align: center; }
        .status-banner.success { background-color: #d4edda; border: 1px solid #c3e6cb; color: #155724; }
        .status-banner.warning { background-color: #fff3cd; border: 1px solid #ffeaa7; color: #856404; }
        .status-banner.danger { background-color: #f8d7da; border: 1px solid #f5c6cb; color: #721c24; }
        .stats-grid { display: grid; grid-template-columns: repeat(auto-fit, minmax(200px, 1fr)); gap: 20px; margin-bottom: 30px; }
        .stat-card { background: white; padding: 20px; border-radius: 8px; box-shadow: 0 2px 4px rgba(0,0,0,0.1); text-align: center; }
        .stat-number { font-size: 2em; font-weight: bold; color: #2c3e50; }
        .stat-label { color: #7f8c8d; margin-top: 5px; }
        .platforms-grid { display: grid; grid-template-columns: repeat(auto-fit, minmax(300px, 1fr)); gap: 20px; margin-bottom: 30px; }
        .platform-card { background: white; border-radius: 8px; box-shadow: 0 2px 4px rgba(0,0,0,0.1); overflow: hidden; }
        .platform-header { padding: 15px; font-weight: bold; color: white; }
        .platform-header.linux { background-color: #e74c3c; }
        .platform-header.windows { background-color: #3498db; }
        .platform-header.macos { background-color: #95a5a6; }
        .platform-body { padding: 20px; }
        .platform-status { display: inline-block; padding: 4px 12px; border-radius: 20px; font-size: 12px; font-weight: bold; }
        .platform-status.success { background-color: #d4edda; color: #155724; }
        .platform-status.failed { background-color: #f8d7da; color: #721c24; }
        .platform-status.unknown { background-color: #e2e3e5; color: #6c757d; }
        .details-section { background: white; padding: 20px; border-radius: 8px; box-shadow: 0 2px 4px rgba(0,0,0,0.1); }
        .json-data { background-color: #f8f9fa; padding: 15px; border-radius: 4px; overflow-x: auto; font-family: 'Courier New', monospace; font-size: 12px; }
        .footer { text-align: center; margin-top: 40px; color: #7f8c8d; }
    </style>
</head>
<body>
    <div class="container">
        <div class="header">
            <h1>🐳 Cross-Platform Audio Capture</h1>
            <div class="subtitle">Docker Testing Dashboard</div>
        </div>
        
        <div class="status-banner $status_class">
            <h2>$status_icon $status_text</h2>
            <p>Generated: $timestamp</p>
        </div>
        
        <div class="stats-grid">
            <div class="stat-card">
                <div class="stat-number">3</div>
                <div class="stat-label">Total Platforms</div>
            </div>
            <div class="stat-card">
                <div class="stat-number">$successful_count</div>
                <div class="stat-label">Successful</div>
            </div>
            <div class="stat-card">
                <div class="stat-number">$failed_count</div>
                <div class="stat-label">Failed</div>
            </div>
        </div>
        
        <div class="platforms-grid">
EOF
    
    # Add platform cards
    for platform in "linux" "windows" "macos"; do
        local platform_data=$(jq -r ".platforms.$platform" "$json_file")
        local platform_status=$(echo "$platform_data" | jq -r '.compilation // .status // "unknown"')
        local platform_icon=""
        local platform_name=""
        
        case "$platform" in
            "linux")
                platform_icon="🐧"
                platform_name="Linux"
                ;;
            "windows")
                platform_icon="🪟"
                platform_name="Windows"
                ;;
            "macos")
                platform_icon="🍎"
                platform_name="macOS"
                ;;
        esac
        
        local status_class_platform="unknown"
        case "$platform_status" in
            "success") status_class_platform="success" ;;
            "failed"|"error") status_class_platform="failed" ;;
        esac
        
        cat >> "$output_file" << EOF
            <div class="platform-card">
                <div class="platform-header $platform">
                    $platform_icon $platform_name
                </div>
                <div class="platform-body">
                    <div style="margin-bottom: 10px;">
                        <span class="platform-status $status_class_platform">$platform_status</span>
                    </div>
                    <div style="font-size: 12px; color: #6c757d;">
                        <pre>$(echo "$platform_data" | jq . 2>/dev/null || echo "No data available")</pre>
                    </div>
                </div>
            </div>
EOF
    done
    
    cat >> "$output_file" << EOF
        </div>
        
        <div class="details-section">
            <h3>Raw Test Data</h3>
            <div class="json-data">
                <pre>$(cat "$json_file" | jq . 2>/dev/null || echo "Invalid JSON data")</pre>
            </div>
        </div>
        
        <div class="footer">
            <p>Generated by Rust Cross-Platform Audio Capture Testing Suite</p>
            <p>Last updated: $timestamp</p>
        </div>
    </div>
</body>
</html>
EOF
    
    log_success "HTML dashboard generated: $output_file"
}

# Main execution
main() {
    local output_format="both"
    local output_dir="$RESULTS_DIR/reports"
    
    # Parse command line arguments
    while [[ $# -gt 0 ]]; do
        case $1 in
            --format)
                output_format="$2"
                shift 2
                ;;
            --output-dir)
                output_dir="$2"
                shift 2
                ;;
            --help)
                echo "Usage: $0 [--format FORMAT] [--output-dir DIR]"
                echo "  --format FORMAT      Output format: json, html, or both (default: both)"
                echo "  --output-dir DIR     Output directory (default: test-results/reports)"
                exit 0
                ;;
            *)
                log_error "Unknown option: $1"
                exit 1
                ;;
        esac
    done
    
    log_info "🔄 Aggregating test results..."
    
    # Create output directory
    mkdir -p "$output_dir"
    
    # Generate reports based on format
    case "$output_format" in
        "json")
            generate_json_report "$output_dir/aggregated_results_${TIMESTAMP}.json"
            ;;
        "html")
            local json_file="$output_dir/aggregated_results_${TIMESTAMP}.json"
            generate_json_report "$json_file"
            generate_html_dashboard "$json_file" "$output_dir/dashboard_${TIMESTAMP}.html"
            ;;
        "both")
            local json_file="$output_dir/aggregated_results_${TIMESTAMP}.json"
            generate_json_report "$json_file"
            generate_html_dashboard "$json_file" "$output_dir/dashboard_${TIMESTAMP}.html"
            ;;
        *)
            log_error "Unknown format: $output_format"
            exit 1
            ;;
    esac
    
    log_success "✅ Test result aggregation completed!"
    log_info "📊 Reports available in: $output_dir"
}

# Run main function with all arguments
main "$@"
