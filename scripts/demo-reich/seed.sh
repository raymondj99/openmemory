#!/usr/bin/env bash
# Seed openmemory and start the MCP HTTP server plus watcher for the demo.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
DEMO_ROOT="${DEMO_ROOT:-/tmp/openmemory-reich-demo}"
OPENMEMORY_HOME="${OPENMEMORY_HOME:-$DEMO_ROOT/om-home}"
OM="${OM:-$REPO_ROOT/target/release/openmemory}"
MCP_ADDR="${MCP_ADDR:-127.0.0.1:7801}"
MCP_URL="http://$MCP_ADDR/mcp"
ARTIFACTS="$DEMO_ROOT/artifacts"

mkdir -p "$ARTIFACTS" "$OPENMEMORY_HOME"

if [[ ! -x "$OM" ]]; then
    printf 'error: openmemory binary not executable: %s\n' "$OM" >&2
    printf 'hint: run cargo build --release --bin openmemory first\n' >&2
    exit 1
fi

stop_pid_file() {
    local path="$1"
    if [[ -f "$path" ]]; then
        local pid
        pid="$(cat "$path")"
        if [[ -n "$pid" ]] && kill -0 "$pid" 2>/dev/null; then
            kill "$pid" 2>/dev/null || true
        fi
        rm -f "$path"
    fi
}

mcp_call() {
    local method="$1" params="$2"
    curl -s -X POST "$MCP_URL" \
        -H 'Content-Type: application/json' \
        -d "$(jq -n --arg m "$method" --argjson p "$params" \
              '{jsonrpc:"2.0",id:1,method:$m,params:$p}')"
}

index_text() {
    local uri="$1" path="$2"
    mcp_call tools/call "$(jq -n --arg u "$uri" --rawfile t "$path" \
        '{name:"openmemory_index_text",arguments:{uri:$u,text:$t}}')" \
        > /dev/null
    printf 'indexed %s\n' "$uri"
}

remember() {
    "$OM" --home "$OPENMEMORY_HOME" remember "$@"
}

"$OM" --home "$OPENMEMORY_HOME" init

stop_pid_file "$ARTIFACTS/mcp.pid"
OPENMEMORY_HTTP_TOKEN= "$OM" --home "$OPENMEMORY_HOME" mcp --http "$MCP_ADDR" \
    > "$ARTIFACTS/mcp.log" 2>&1 &
echo $! > "$ARTIFACTS/mcp.pid"
until curl -sf "http://$MCP_ADDR/healthz" > /dev/null; do
    sleep 0.2
done

remember "AADR_v54_1" \
    --entity-type fact --source "demo-seed" \
    --observation "Path: $DEMO_ROOT/labshare/data/aadr/v54.1" \
    --observation "Format: EIGENSTRAT (.geno, .snp, .ind)" \
    --observation "Published 2023-04; supersedes v52.2"

remember "SNP_panel_1240K" \
    --entity-type fact --source "demo-seed" \
    --observation "1,233,013 targeted SNPs" \
    --observation "Chromosome encoding in .snp column 2: X=23, Y=24, autosomes 1-22" \
    --observation "Capture reagent described in Mathieson et al. 2015"

remember "SNP_panel_1240K_X" \
    --entity-type fact --source "demo-seed" \
    --observation "Path: $DEMO_ROOT/labshare/data/panels/1240K_chrX.bed" \
    --observation "Derived from 1240K .snp by awk '\$2==23' then BED conversion" \
    --relation "derived_from=SNP_panel_1240K:fact"

remember "SNP_panel_1240K_Y" \
    --entity-type fact --source "demo-seed" \
    --observation "Path: $DEMO_ROOT/labshare/data/panels/1240K_chrY.bed" \
    --relation "derived_from=SNP_panel_1240K:fact"

remember "hg19" \
    --entity-type fact --source "demo-seed" \
    --observation "Reference genome used for all Lower Rhine-Meuse BAMs" \
    --observation "Olalde 2026 Methods, Bioinformatics section"

remember "samtools" \
    --entity-type tool --source "demo-seed" \
    --observation "Used for read counting" \
    --observation "Lab default: samtools view -c -q 30 -L <bed> <bam> to count reads overlapping a position set"

remember "bwa_0_7_15" \
    --entity-type tool --source "demo-seed" \
    --observation "Aligner used by the lab pipeline (bwa samse mode)"

remember "SeqPrep_v1_1" \
    --entity-type tool --source "demo-seed" \
    --observation "Adapter merging, 15-bp overlap rule"

remember "MolecularSexProtocol" \
    --entity-type preference --source "demo-seed" \
    --observation "Ry = n_reads_on_Y_SNPs / (n_reads_on_X_SNPs + n_reads_on_Y_SNPs)" \
    --observation "Threshold female: Ry < 0.03" \
    --observation "Threshold male: Ry > 0.32" \
    --observation "Min reads on sex chromosomes: 200. Below that, do not call; output low_coverage." \
    --observation "Ambiguous (0.03 <= Ry <= 0.32): XX? if Ry < 0.15 else XY?. Lab convention; not in Olalde 2026." \
    --observation "Counting strategy: any read overlapping a panel position (samtools -L), not strict base coverage." \
    --relation "cites=Olalde2026_RhineMeuse:fact" \
    --relation "supersedes=Skoglund2013_SexProtocol:fact"

remember "Olalde2026_RhineMeuse" \
    --entity-type fact --source "demo-seed" \
    --observation "Nature 2026, doi:10.1038/s41586-026-10111-8" \
    --observation "Source of the 0.03 / 0.32 thresholds for sex calls"

remember "Skoglund2013_SexProtocol" \
    --entity-type fact --source "demo-seed" \
    --observation "Original Ry method; used a 0.075 cutoff for female calls" \
    --observation "Deprecated by our lab on 2026-05-01 in favour of Olalde 2026 thresholds"

remember "LabPipelineDefaults" \
    --entity-type preference --source "demo-seed" \
    --observation "min_reads_sex: 200" \
    --observation "output_tsv_columns: sample_id, n_reads_X, n_reads_Y, Ry, sex_call" \
    --observation "results_dir: $DEMO_ROOT/labshare/results/sex" \
    --observation "input_bam_dir for current batch: $DEMO_ROOT/labshare/bams/2026-05"

for sid in I12091 I12902 I15651 I33738 I38442; do
    remember "$sid" \
        --entity-type fact --source "demo-seed" \
        --observation "Sample in the Lower Rhine-Meuse batch reported in Olalde 2026" \
        --relation "aligned_to=hg19:fact" \
        --relation "enriched_with=SNP_panel_1240K:fact" \
        --relation "reported_in=Olalde2026_RhineMeuse:fact"
done

remember "Raymond" \
    --entity-type person --source "demo-seed" \
    --observation "Maintainer of the molecular-sex protocol and the openmemory project" \
    --relation "maintains=MolecularSexProtocol:preference" \
    --relation "maintains=openmemory:project"

ALIAS_LOG="$ARTIFACTS/alias-normalization.jsonl"
: > "$ALIAS_LOG"
for name in "1240K capture" "1240K" "1,240K SNP panel" "1240K_capture"; do
    remember "$name" \
        --entity-type fact --source "demo-seed" \
        --observation "Alias variant for the 1240K capture panel" \
        --json | tee -a "$ALIAS_LOG" > /dev/null
done
if ! grep -q '"normalized"' "$ALIAS_LOG"; then
    printf 'warning: alias normalization did not emit a normalized field\n' >&2
fi

index_text "paper://olalde-2026-rhine-meuse#methods-sex" \
    "$DEMO_ROOT/fixtures/paper-olalde-2026-methods-sex.txt"
index_text "paper://olalde-2026-rhine-meuse#methods-bioinfo" \
    "$DEMO_ROOT/fixtures/paper-olalde-2026-methods-bioinfo.txt"
index_text "paper://skoglund-2013-sex-determination" \
    "$DEMO_ROOT/fixtures/paper-skoglund-2013-sex.txt"
index_text "decision://sex-thresholds-olalde-2026" \
    "$DEMO_ROOT/fixtures/decision-sex-thresholds-olalde-2026.md"
index_text "decision://min-reads-sex-200" \
    "$DEMO_ROOT/fixtures/decision-min-reads-sex-200.md"
index_text "decision://wrong-panel-2023" \
    "$DEMO_ROOT/fixtures/decision-wrong-panel-2023.md"
index_text "protocol://molecular-sex" \
    "$DEMO_ROOT/labshare/protocols/molecular-sex.md"
index_text "protocol://1240K-panel-layout" \
    "$DEMO_ROOT/labshare/protocols/1240K-panel-layout.md"
index_text "protocol://bam-readcount" \
    "$DEMO_ROOT/labshare/protocols/bam-readcount.md"
index_text "dataset-readme://1240K" \
    "$DEMO_ROOT/labshare/data/aadr/v54.1/README.md"
index_text "paper://mtdna-contamination" \
    "$DEMO_ROOT/fixtures/paper-mtdna-redherring.txt"

remember "MolecularSexProtocol" \
    --entity-type preference --source "stale-2024-note" \
    --observation "Historical: through 2024-11 the lab used the Skoglund 0.075 cutoff for female calls." \
    --confidence 0.6

stop_pid_file "$ARTIFACTS/watch.pid"
"$OM" --home "$OPENMEMORY_HOME" watch "$DEMO_ROOT/labshare" \
    --exts md,sh,py \
    > "$ARTIFACTS/watch.log" 2>&1 &
echo $! > "$ARTIFACTS/watch.pid"
until grep -q "openmemory watch: watching" "$ARTIFACTS/watch.log" 2>/dev/null; do
    sleep 0.2
done
sleep 2

printf 'seeded openmemory demo at %s\n' "$DEMO_ROOT"
printf 'mcp: http://%s/mcp\n' "$MCP_ADDR"
