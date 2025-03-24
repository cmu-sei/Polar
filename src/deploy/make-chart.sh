#!/bin/bash

# Function to print usage information
usage() {
    echo "Usage: $0 <directory> <chart-name>"
    echo "  <directory>   Path to the directory containing Dhall files"
    echo "  <chart-name>  Name of the Helm chart to be created"
    exit 1
}

# Ensure both arguments are provided
if [ $# -lt 2 ]; then
    echo "Error: Missing required arguments."
    usage
fi

DIR="$1"
CHART_NAME="$2"
HELM_DIR="$DIR/$CHART_NAME"

# Ensure necessary tools are installed
for cmd in dhall-to-yaml kubectl helm; do
    if ! command -v "$cmd" &> /dev/null; then
        echo "Error: $cmd is not installed. Install it and try again." >&2
        exit 1
    fi
done

# Create Helm chart structure if it doesn’t exist

if [ ! -d "$HELM_DIR" ]; then
    echo "🔧 Creating Helm chart directory: $HELM_DIR"
    mkdir -p "$HELM_DIR/templates"
fi

# Ensure Chart.yaml exists
if [ ! -f "$HELM_DIR/Chart.yaml" ]; then
    cat <<EOF > "$HELM_DIR/Chart.yaml"
### CAUTION! THIS CHART AND THE TEMPLATES WITHIN IT ARE PRE-GENERATED AND ARE NOT INTENDED TO BE MANUALLY EDITED ###
apiVersion: v2
name: $CHART_NAME
description: A pre-generated helm chart for deploying a Polar service.
type: application
version: 0.1.0
appVersion: "0.1.0"
EOF
    echo "✅ Created $HELM_DIR/Chart.yaml"
fi

# Process Dhall files
for dhall_file in "$DIR"/*.dhall; do
    [ -e "$dhall_file" ] || continue  # Skip if no files found

    yaml_file="${dhall_file%.dhall}.yaml"
    template_file="$HELM_DIR/templates/$(basename "$yaml_file")"

    echo "🔄 Converting $dhall_file -> $template_file"

    if ! dhall-to-yaml --file "$dhall_file" > "$template_file"; then
        echo "❌ Error: Failed to convert $dhall_file to YAML" >&2
        continue
    fi

    echo "✅ Successfully created $template_file"

    # Pre-flight check: Validate YAML with Kubernetes
    echo "🔍 Validating $template_file with kubectl..."
    if ! kubectl apply --dry-run=client -f "$template_file"; then
        echo "❌ Error: $template_file is invalid for Kubernetes" >&2
        continue
    fi

    echo "✅ $template_file is valid for Kubernetes"
done

# Run Helm linting
echo "🔎 Running Helm lint check..."
if ! helm lint "$HELM_DIR"; then
    echo "❌ Helm linting failed!" >&2
    exit 1
fi

# Render Helm template
echo "🛠️ Rendering Helm template..."
if ! helm template "$HELM_DIR"; then
    echo "❌ Helm template rendering failed!" >&2
    exit 1
fi

echo "🚀 Helm chart setup complete. You can now run:"
echo "   helm install polar-neo4j $HELM_DIR"

