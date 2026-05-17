//! `openmemory eval`: run a retrieval-quality benchmark and print a
//! summary.
//!
//! The command wraps [`openmemory_eval::EvalRunner`]. It selects a
//! dataset adapter by name (`longmem-s` or `coding-mem`), points it at
//! a fixture directory, runs the harness with the requested search
//! mode and top-K, and prints a text summary. An optional `--report
//! <path>` writes the same data as JSON so CI can pin the numbers.
//! When `--baseline <path>` is supplied, the run also prints per-
//! metric deltas against the baseline.

use std::fs::File;
use std::io::Write;
use std::path::Path;
#[cfg(feature = "embeddings")]
use std::sync::Arc;

use anyhow::{bail, Context, Result};
#[cfg(feature = "embeddings")]
use openmemory_core::config::Config;
use openmemory_eval::coding_mem::CodingMemDataset;
use openmemory_eval::longmem_s::LongMemSDataset;
use openmemory_eval::{EvalRunner, RunnerConfig};
use openmemory_index::SearchMode;

use crate::cli::EvalArgs;

pub fn run(args: EvalArgs) -> Result<()> {
    let mode = parse_mode(&args.mode)?;
    let config = RunnerConfig {
        mode,
        top_k: args.k,
    };
    let runner = attach_embedder_if_needed(EvalRunner::new(config), mode)?;

    let report = match args.dataset.as_str() {
        "longmem-s" => {
            let ds = LongMemSDataset::from_path(&args.dataset_path)
                .context("loading longmem-s dataset")?;
            runner.run(&ds)?
        }
        "coding-mem" => {
            let ds = CodingMemDataset::from_path(&args.dataset_path)
                .context("loading coding-mem dataset")?;
            runner.run(&ds)?
        }
        other => bail!("unknown dataset: {other:?} (expected `longmem-s` or `coding-mem`)"),
    };

    print_summary(&report);

    if let Some(path) = args.report.as_deref() {
        write_json(path, &report)?;
        println!("\nWrote JSON report to {}", path.display());
    }

    if let Some(baseline) = args.baseline.as_deref() {
        let prev = load_baseline(baseline)
            .with_context(|| format!("loading baseline report {}", baseline.display()))?;
        if prev.dataset != report.dataset {
            bail!(
                "baseline dataset {:?} does not match current dataset {:?}; refusing to diff",
                prev.dataset,
                report.dataset
            );
        }
        if prev.n_queries != report.n_queries {
            bail!(
                "baseline has {} queries but current run has {}; refusing to diff",
                prev.n_queries,
                report.n_queries
            );
        }
        print_delta(&prev, &report);
    }

    Ok(())
}

fn mode_needs_embedder(mode: SearchMode) -> bool {
    matches!(mode, SearchMode::Hybrid | SearchMode::VectorOnly)
}

#[cfg(feature = "embeddings")]
fn attach_embedder_if_needed(runner: EvalRunner, mode: SearchMode) -> Result<EvalRunner> {
    if !mode_needs_embedder(mode) {
        return Ok(runner);
    }
    let models_dir = Config::models_dir().context("resolving models directory")?;
    let Some(embedder) = openmemory_embed::load_embedder(&models_dir) else {
        bail!(
            "openmemory eval --mode {} requires a downloaded embedding model; run \
             `openmemory model download` or use `--mode keyword`",
            mode_label(mode)
        );
    };
    eprintln!(
        "openmemory eval: embeddings active ({})",
        embedder.model_name()
    );
    Ok(runner.with_embedder(Arc::new(embedder)))
}

#[cfg(not(feature = "embeddings"))]
fn attach_embedder_if_needed(runner: EvalRunner, mode: SearchMode) -> Result<EvalRunner> {
    if mode_needs_embedder(mode) {
        bail!(
            "openmemory eval --mode {} requires embeddings; rebuild with \
             `--features eval,embeddings` or use `--mode keyword`",
            mode_label(mode)
        );
    }
    Ok(runner)
}

fn mode_label(mode: SearchMode) -> &'static str {
    match mode {
        SearchMode::Hybrid => "hybrid",
        SearchMode::KeywordOnly => "keyword",
        SearchMode::VectorOnly => "vector",
    }
}

fn parse_mode(s: &str) -> Result<SearchMode> {
    match s {
        "hybrid" => Ok(SearchMode::Hybrid),
        "keyword" => Ok(SearchMode::KeywordOnly),
        "vector" => Ok(SearchMode::VectorOnly),
        other => bail!("unknown mode: {other:?} (expected `hybrid`, `keyword`, or `vector`)"),
    }
}

fn print_summary(report: &openmemory_eval::RetrievalReport) {
    println!("openmemory eval");
    println!("  dataset         : {}", report.dataset);
    println!("  queries         : {}", report.n_queries);
    println!("  R@5             : {:.4}", report.metrics.r_at_5);
    println!("  R@10            : {:.4}", report.metrics.r_at_10);
    println!("  MRR             : {:.4}", report.metrics.mrr);
    println!("  NDCG@5          : {:.4}", report.metrics.ndcg_at_5);
    println!("  NDCG@10         : {:.4}", report.metrics.ndcg_at_10);
    println!("  mean latency ms : {:.2}", report.mean_latency_ms);
}

fn write_json(path: &Path, report: &openmemory_eval::RetrievalReport) -> Result<()> {
    let mut f = File::create(path).with_context(|| format!("creating {}", path.display()))?;
    let body = serde_json::to_string_pretty(report).context("serialising report")?;
    f.write_all(body.as_bytes())
        .with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

fn load_baseline(path: &Path) -> Result<openmemory_eval::RetrievalReport> {
    let body =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let report: openmemory_eval::RetrievalReport =
        serde_json::from_str(&body).context("parsing baseline JSON")?;
    Ok(report)
}

fn print_delta(prev: &openmemory_eval::RetrievalReport, now: &openmemory_eval::RetrievalReport) {
    println!("\nbaseline diff (now - baseline):");
    let m = &now.metrics;
    let p = &prev.metrics;
    println!("  R@5     : {:+.4}", m.r_at_5 - p.r_at_5);
    println!("  R@10    : {:+.4}", m.r_at_10 - p.r_at_10);
    println!("  MRR     : {:+.4}", m.mrr - p.mrr);
    println!("  NDCG@5  : {:+.4}", m.ndcg_at_5 - p.ndcg_at_5);
    println!("  NDCG@10 : {:+.4}", m.ndcg_at_10 - p.ndcg_at_10);
    println!(
        "  latency : {:+.2} ms",
        now.mean_latency_ms - prev.mean_latency_ms
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_lines(path: &Path, lines: &[&str]) {
        let mut f = File::create(path).unwrap();
        for line in lines {
            writeln!(f, "{line}").unwrap();
        }
    }

    fn fake_dataset_dir() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        write_lines(
            &dir.path().join("corpus.jsonl"),
            &[
                r#"{"uri":"doc://a","text":"alpha document about architecture"}"#,
                r#"{"uri":"doc://b","text":"beta document about performance"}"#,
            ],
        );
        write_lines(
            &dir.path().join("queries.jsonl"),
            &[r#"{"id":"q1","text":"architecture"}"#],
        );
        write_lines(
            &dir.path().join("judgments.jsonl"),
            &[r#"{"query_id":"q1","uri":"doc://a","relevance":1}"#],
        );
        dir
    }

    #[test]
    fn eval_runs_against_longmem_s_fixture() {
        let dir = fake_dataset_dir();
        let report_path = dir.path().join("report.json");
        let args = EvalArgs {
            dataset: "longmem-s".into(),
            dataset_path: dir.path().to_path_buf(),
            mode: "keyword".into(),
            k: 10,
            report: Some(report_path.clone()),
            baseline: None,
        };
        run(args).unwrap();
        let body = std::fs::read_to_string(report_path).unwrap();
        assert!(body.contains("\"dataset\""));
        assert!(body.contains("longmem-s"));
    }

    #[test]
    fn eval_runs_against_coding_mem_fixture() {
        let dir = fake_dataset_dir();
        let args = EvalArgs {
            dataset: "coding-mem".into(),
            dataset_path: dir.path().to_path_buf(),
            mode: "keyword".into(),
            k: 10,
            report: None,
            baseline: None,
        };
        run(args).unwrap();
    }

    #[test]
    fn eval_rejects_unknown_dataset() {
        let dir = fake_dataset_dir();
        let args = EvalArgs {
            dataset: "nope".into(),
            dataset_path: dir.path().to_path_buf(),
            mode: "keyword".into(),
            k: 10,
            report: None,
            baseline: None,
        };
        let err = run(args).unwrap_err();
        assert!(err.to_string().contains("unknown dataset"));
    }

    #[cfg(feature = "embeddings")]
    #[test]
    fn eval_hybrid_requires_downloaded_model() {
        use crate::cli::with_home;

        let home = tempfile::tempdir().unwrap();
        let dataset = fake_dataset_dir();
        with_home(home.path(), || {
            let args = EvalArgs {
                dataset: "longmem-s".into(),
                dataset_path: dataset.path().to_path_buf(),
                mode: "hybrid".into(),
                k: 10,
                report: None,
                baseline: None,
            };
            let err = run(args).unwrap_err();
            assert!(err
                .to_string()
                .contains("requires a downloaded embedding model"));
        });
    }

    #[test]
    fn parse_mode_handles_all_three() {
        assert!(matches!(parse_mode("hybrid"), Ok(SearchMode::Hybrid)));
        assert!(matches!(parse_mode("keyword"), Ok(SearchMode::KeywordOnly)));
        assert!(matches!(parse_mode("vector"), Ok(SearchMode::VectorOnly)));
        assert!(parse_mode("nope").is_err());
    }
}
