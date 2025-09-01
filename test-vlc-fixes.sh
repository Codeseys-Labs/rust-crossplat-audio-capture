#!/bin/bash

# Script to commit, push, and test VLC capture fixes
set -e

echo "🚀 Testing VLC Capture Fixes"
echo "============================"

# Check if we're in a git repository
if [ ! -d ".git" ]; then
    echo "❌ Not in a git repository"
    exit 1
fi

# Check if gh CLI is available
if ! command -v gh >/dev/null 2>&1; then
    echo "❌ GitHub CLI (gh) not found. Please install it first."
    exit 1
fi

# Get current branch
CURRENT_BRANCH=$(git branch --show-current)
echo "📍 Current branch: $CURRENT_BRANCH"

# Check for uncommitted changes
if ! git diff --quiet || ! git diff --cached --quiet; then
    echo "📝 Committing changes..."
    
    # Add all changes
    git add .
    
    # Create commit message
    COMMIT_MSG="Fix VLC capture setup in GitHub Actions

- Fix PipeWire loopback device creation using module loading
- Simplify VLC startup command for better CI compatibility  
- Add multiple test URLs with local audio fallback
- Improve VLC node detection and audio routing
- Enhanced monitoring and diagnostics
- Better error handling and logging

Addresses issues:
- 'Host is down' error when creating loopback devices
- VLC exiting early due to interface failures
- Missing audio activity on virtual devices"

    git commit -m "$COMMIT_MSG"
    echo "✅ Changes committed"
else
    echo "ℹ️  No uncommitted changes found"
fi

# Push changes
echo "📤 Pushing to remote..."
git push origin "$CURRENT_BRANCH"
echo "✅ Changes pushed"

# Trigger GitHub Actions workflow
echo "🔄 Triggering GitHub Actions workflow..."
WORKFLOW_NAME="Linux PipeWire Tests"

# Check if workflow exists
if ! gh workflow list | grep -q "$WORKFLOW_NAME"; then
    echo "❌ Workflow '$WORKFLOW_NAME' not found"
    echo "Available workflows:"
    gh workflow list
    exit 1
fi

# Trigger the workflow
echo "Triggering workflow: $WORKFLOW_NAME"
gh workflow run "$WORKFLOW_NAME"

# Wait a moment for the run to start
echo "⏳ Waiting for workflow run to start..."
sleep 10

# Get the latest run ID
RUN_ID=$(gh run list --workflow="$WORKFLOW_NAME" --limit=1 --json databaseId --jq '.[0].databaseId')
echo "📊 Latest run ID: $RUN_ID"

# Monitor the workflow
echo "👀 Monitoring workflow progress..."
echo "You can also view it at: https://github.com/$(gh repo view --json owner,name --jq '.owner.login + "/" + .name')/actions/runs/$RUN_ID"

# Wait for completion (with timeout)
TIMEOUT=1800  # 30 minutes
ELAPSED=0
INTERVAL=30

while [ $ELAPSED -lt $TIMEOUT ]; do
    STATUS=$(gh run view "$RUN_ID" --json status --jq '.status')
    CONCLUSION=$(gh run view "$RUN_ID" --json conclusion --jq '.conclusion')
    
    echo "[$((ELAPSED/60))m] Status: $STATUS, Conclusion: $CONCLUSION"
    
    if [ "$STATUS" = "completed" ]; then
        echo "🏁 Workflow completed with conclusion: $CONCLUSION"
        break
    fi
    
    sleep $INTERVAL
    ELAPSED=$((ELAPSED + INTERVAL))
done

if [ $ELAPSED -ge $TIMEOUT ]; then
    echo "⏰ Timeout reached. Workflow may still be running."
    echo "Check status manually: gh run view $RUN_ID"
fi

# Download logs
echo "📥 Downloading workflow logs..."
LOG_DIR="workflow-logs-$(date +%Y%m%d-%H%M%S)"
mkdir -p "$LOG_DIR"

# Download all logs
gh run download "$RUN_ID" --dir "$LOG_DIR" || {
    echo "⚠️  Could not download artifacts, trying to get logs directly..."
    gh run view "$RUN_ID" --log > "$LOG_DIR/workflow-output.log"
}

# Create a summary file
SUMMARY_FILE="$LOG_DIR/test-summary.md"
cat > "$SUMMARY_FILE" << EOF
# VLC Capture Fix Test Results

**Run ID:** $RUN_ID  
**Branch:** $CURRENT_BRANCH  
**Date:** $(date)  
**Status:** $STATUS  
**Conclusion:** $CONCLUSION  

## Workflow URL
https://github.com/$(gh repo view --json owner,name --jq '.owner.login + "/" + .name')/actions/runs/$RUN_ID

## Key Changes Tested
- Fixed PipeWire loopback device creation using module loading
- Simplified VLC startup command for better CI compatibility
- Added multiple test URLs with local audio fallback  
- Improved VLC node detection and audio routing
- Enhanced monitoring and diagnostics
- Better error handling and logging

## Files to Check
- Look for VLC logs in artifacts
- Check if vlc_dynamic_capture.wav was created
- Review PipeWire diagnostics output
- Examine any error messages in the logs

## Next Steps
EOF

if [ "$CONCLUSION" = "success" ]; then
    echo "- ✅ Test passed! VLC capture should now be working" >> "$SUMMARY_FILE"
elif [ "$CONCLUSION" = "failure" ]; then
    echo "- ❌ Test failed. Review logs to identify remaining issues" >> "$SUMMARY_FILE"
    echo "- Check VLC startup logs for errors" >> "$SUMMARY_FILE"
    echo "- Verify PipeWire loopback device creation" >> "$SUMMARY_FILE"
    echo "- Look for audio activity detection results" >> "$SUMMARY_FILE"
else
    echo "- ⏳ Test still running or status unclear" >> "$SUMMARY_FILE"
fi

echo ""
echo "📋 Test Summary"
echo "==============="
cat "$SUMMARY_FILE"

echo ""
echo "📁 Logs saved to: $LOG_DIR"
echo "📄 Summary file: $SUMMARY_FILE"

# List downloaded files
echo ""
echo "📦 Downloaded files:"
find "$LOG_DIR" -type f -exec ls -lh {} \;

echo ""
echo "🔍 To analyze the logs, you can:"
echo "   - Check $SUMMARY_FILE for overview"
echo "   - Look in $LOG_DIR/ for detailed logs"
echo "   - Search for 'VLC' or 'test-audio' in the logs"
echo "   - Check for any .wav files in the artifacts"

echo ""
echo "✅ Test completed! Please review the logs and summary."
