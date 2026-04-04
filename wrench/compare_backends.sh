#!/bin/bash
# Compare GL (ANGLE) and wgpu backend rendering for all unique YAML files
# referenced in reftest.list files. Outputs a per-file diff summary.
#
# Usage: cd wrench && bash compare_backends.sh [category]
#   category: optional filter (e.g., "blend", "gradient") — empty = all
#
# Requires: ImageMagick (for `compare`/`identify`), wrench built with wgpu_backend

set -euo pipefail

WRENCH="../target/debug/wrench.exe"
REFTEST_DIR="reftests"
OUT_DIR="/tmp/wr_compare"
FILTER="${1:-}"

mkdir -p "$OUT_DIR/gl" "$OUT_DIR/wgpu" "$OUT_DIR/diff"

# Collect all unique YAML files from reftest.list files
collect_yamls() {
    for listfile in "$REFTEST_DIR"/*/reftest.list; do
        dir=$(dirname "$listfile")
        # Extract .yaml filenames from reftest.list lines
        grep -oE '[a-zA-Z0-9_/.-]+\.yaml' "$listfile" 2>/dev/null | while read -r yaml; do
            echo "$dir/$yaml"
        done
    done | sort -u
}

YAMLS=$(collect_yamls)
TOTAL=$(echo "$YAMLS" | wc -l)

if [ -n "$FILTER" ]; then
    YAMLS=$(echo "$YAMLS" | grep "/$FILTER/")
    TOTAL=$(echo "$YAMLS" | wc -l)
    echo "Filtered to category '$FILTER': $TOTAL YAML files"
fi

echo "=== Backend Comparison: $TOTAL YAML files ==="
echo ""

PASS=0
FAIL=0
ERROR_GL=0
ERROR_WGPU=0
SKIP=0
idx=0

# Results file
RESULTS="$OUT_DIR/results.txt"
> "$RESULTS"

echo "$YAMLS" | while read -r yaml; do
    idx=$((idx + 1))

    # Create safe output filename
    safename=$(echo "$yaml" | sed 's|[/\\]|_|g' | sed 's|\.yaml$||')
    gl_png="$OUT_DIR/gl/${safename}.png"
    wgpu_png="$OUT_DIR/wgpu/${safename}.png"
    diff_png="$OUT_DIR/diff/${safename}.png"

    # Skip if already done
    if [ -f "$gl_png" ] && [ -f "$wgpu_png" ]; then
        : # re-compare below
    else
        # Render with GL (ANGLE)
        gl_ok=true
        if ! timeout 15 "$WRENCH" --angle png "$yaml" "$gl_png" >/dev/null 2>&1; then
            gl_ok=false
        fi

        # Render with wgpu
        wgpu_ok=true
        if ! timeout 15 "$WRENCH" --wgpu png "$yaml" "$wgpu_png" >/dev/null 2>&1; then
            wgpu_ok=false
        fi

        if ! $gl_ok && ! $wgpu_ok; then
            echo "BOTH_ERROR  $yaml" >> "$RESULTS"
            SKIP=$((SKIP + 1))
            continue
        elif ! $gl_ok; then
            echo "GL_ERROR    $yaml" >> "$RESULTS"
            ERROR_GL=$((ERROR_GL + 1))
            continue
        elif ! $wgpu_ok; then
            echo "WGPU_ERROR  $yaml" >> "$RESULTS"
            ERROR_WGPU=$((ERROR_WGPU + 1))
            continue
        fi
    fi

    # Compare using ImageMagick if available, else fall back to cmp
    if command -v magick >/dev/null 2>&1; then
        # Use ImageMagick compare — returns AE (absolute error) pixel count
        ae=$(magick compare -metric AE "$gl_png" "$wgpu_png" "$diff_png" 2>&1 || true)
        if [ "$ae" = "0" ]; then
            echo "MATCH       $yaml" >> "$RESULTS"
            PASS=$((PASS + 1))
        else
            echo "DIFF($ae)   $yaml" >> "$RESULTS"
            FAIL=$((FAIL + 1))
        fi
    else
        # Fallback: binary comparison
        if cmp -s "$gl_png" "$wgpu_png"; then
            echo "MATCH       $yaml" >> "$RESULTS"
            PASS=$((PASS + 1))
        else
            echo "DIFF        $yaml" >> "$RESULTS"
            FAIL=$((FAIL + 1))
        fi
    fi

    # Progress
    if [ $((idx % 50)) -eq 0 ]; then
        echo "  [$idx/$TOTAL] ..."
    fi
done

echo ""
echo "=== Results ==="
echo "Total:      $TOTAL"
echo "Match:      $PASS"
echo "Diff:       $FAIL"
echo "GL error:   $ERROR_GL"
echo "wgpu error: $ERROR_WGPU"
echo "Both error: $SKIP"
echo ""
echo "Details in: $OUT_DIR/results.txt"
echo "Diff images in: $OUT_DIR/diff/"

# Sort results by type for easy reading
sort "$RESULTS" > "$RESULTS.sorted"
mv "$RESULTS.sorted" "$RESULTS"
