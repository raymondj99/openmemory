//! Categorised query sets for recall-path evaluation.
//!
//! The existing [`crate::dataset::Dataset`] shape describes a corpus the
//! harness ingests itself. Evaluating retrieval *quality* on a store that
//! already exists needs a different input: just the questions, their
//! expected answers, and enough structure to slice the results.
//!
//! Three things this adds over the flat dataset shape:
//!
//! - **Categories.** A single aggregate number hides that a change helped
//!   direct lookup and broke multi-hop. Every query declares its category
//!   and the report rolls up per category.
//! - **Abstention.** A query may legitimately have *no* right answer. A
//!   memory system that confidently returns something anyway poisons the
//!   agent context that consumes it, so "returns nothing" is scored, not
//!   ignored.
//! - **Provenance of the judgment itself.** Judgments mined automatically
//!   from a corpus are evidence of a different quality than judgments a
//!   human confirmed. They are recorded separately so a generated-judgment
//!   artefact cannot quietly decide a design question.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

/// Where a judgment came from. Reported separately so automatic mining
/// cannot silently outvote human review.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum JudgmentSource {
    /// Derived mechanically from corpus structure.
    #[default]
    Generated,
    /// Independently adjudicated by a language model that did not
    /// generate the judgment, against written criteria, with its verdict
    /// and reasoning recorded per query.
    ///
    /// A separate tier, deliberately, and it is worth being precise
    /// about why rather than folding it into either neighbour.
    ///
    /// It is **more** than `Generated`: a mined judgment asserts only
    /// that the corpus structure linked two things, and this asserts
    /// that a reader who was not told the answer agreed the document
    /// answers the question. That catches the failure mode mined
    /// judgments actually have — a correct document graded 0 because the
    /// miner did not know about it, which is exactly the defect the
    /// temporal doc-section chains hit.
    ///
    /// It is **less** than `HumanReviewed`, and not by a little. An
    /// adjudicator shares the generator's blind spots: both read the
    /// same text with the same priors, so a systematically wrong notion
    /// of relevance is confirmed rather than caught. Independence here
    /// is procedural, not statistical. Treat it as a stronger prior,
    /// never as ground truth, and never as a substitute for the human
    /// pass when a design question turns on the answer.
    ModelAdjudicated,
    /// Inspected and confirmed by a human.
    HumanReviewed,
}

impl JudgmentSource {
    /// Stable label used in report rollups and query-set files.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Generated => "generated",
            Self::ModelAdjudicated => "model-adjudicated",
            Self::HumanReviewed => "human-reviewed",
        }
    }

    /// Whether this judgment was checked by anything other than the
    /// process that produced it.
    #[must_use]
    pub fn is_independently_checked(self) -> bool {
        matches!(self, Self::ModelAdjudicated | Self::HumanReviewed)
    }
}

/// A declaration that one query legitimately has more than K right
/// answers, so its raw `R@K` ceiling below 1.0 is a property of the
/// question rather than a defect in the judgments.
///
/// Some questions really do have more answers than a top-K list can
/// hold. "List every value this setting has held" has one right answer
/// per version, and a chain with twelve versions has twelve. The wrong
/// response is to lower the global ceiling threshold until that query
/// fits: one number then authorises every future qrel defect too, and
/// the gate stops distinguishing "this corpus has a broad question" from
/// "somebody judged 30 chunks by accident".
///
/// So the exception is per query, typed, and carries a reason a human
/// wrote. What it buys is narrow: the query is excluded from the raw
/// `R@K` aggregate and from the ceiling statistics. It keeps its NDCG@K
/// and MRR — both remain attainable, because `ndcg_at_k` truncates its
/// ideal DCG at K as well — and it gains
/// [`crate::metrics::capacity_r_at_k`], which normalises by what a
/// K-limited list could hold rather than by the full judgment set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapacityWaiver {
    /// Why this question genuinely has more right answers than K.
    ///
    /// Required and non-empty: a waiver with no stated reason is a
    /// silent cap wearing a different name, and it would be granted by
    /// whoever found the gate inconvenient rather than by whoever
    /// understood the corpus.
    pub reason: String,
}

/// One graded judgment: a document key and how relevant it is.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GradedKey {
    /// Stable document key, resolved to store rows by a
    /// [`crate::resolver::KeyResolver`].
    pub key: String,
    /// Graded relevance in `0..=3`.
    #[serde(default = "default_relevance")]
    pub relevance: u8,
}

fn default_relevance() -> u8 {
    1
}

/// Serde default for [`RecallQuery::judgments_complete`]. Completeness
/// is the assumption; incompleteness is the thing that must be declared.
fn yes() -> bool {
    true
}

/// One evaluation query.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecallQuery {
    pub id: String,
    pub text: String,
    /// Slice label, e.g. `direct`, `multi-hop`, `temporal`, `abstention`.
    pub category: String,
    /// Relevant documents. Empty for an abstention query.
    #[serde(default)]
    pub relevant: Vec<GradedKey>,
    /// Documents removed from the ranking before scoring, because they
    /// are the *source* the query was derived from rather than an answer
    /// to it.
    ///
    /// A generated query is a paraphrase of some document. When that
    /// document is also in the corpus, retrieval finds it first — it
    /// contains the query almost verbatim — and the qrels then face a
    /// choice between two wrong answers: call it irrelevant, and a
    /// correct response scores zero while consuming a top-K slot; call
    /// it relevant, and every query has a free rank-1 hit that measures
    /// nothing. Removing it is the third option, and it is what
    /// known-item and citation-recommendation evaluations do with the
    /// seed document.
    ///
    /// Declared per query rather than inferred in code, so a reader of
    /// the query file can see exactly what was taken out of the ranking.
    /// The count of exclusions actually hit is reported per query, so
    /// the removal cannot quietly grow.
    #[serde(default)]
    pub exclude: Vec<String>,
    /// Declares that this query legitimately has more than K right
    /// answers. See [`CapacityWaiver`]; absent for every ordinary query.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capacity_waiver: Option<CapacityWaiver>,
    /// Whether `relevant` is believed to list **every** document in the
    /// corpus that answers this query.
    ///
    /// True for judgments an external record determines: the commit
    /// record for a subject, the files git says a commit touched, every
    /// version in a mined fact chain. There the corpus itself says what
    /// the answer set is, and a document outside it is outside by
    /// construction.
    ///
    /// False when the judgments come from a *proxy for meaning*. A
    /// `doc-heading` query is judged against the sections carrying that
    /// heading, and the adjudication pass measured what that misses:
    /// `docs/crates.md` answers "MCP server reference" under the heading
    /// "`openmemory-mcp`", and `docs/storage.md`'s "Directory layout"
    /// answers "Store identity and manifests". Neither shares a word
    /// with the query's heading, and no string rule reaches them.
    ///
    /// A false value does not invalidate the slice. It says the recall
    /// family is a **partial-label estimate whose direction is
    /// unknown** — not a lower bound, which is what this said first and
    /// which is wrong.
    ///
    /// Both directions are reachable. Retrieve the one known answer and
    /// miss a hundred unknown ones: reported recall 1.0, true recall
    /// 0.01, so the report is an *upper* bound. Miss the known answer
    /// and retrieve an unjudged correct one: reported 0, true positive,
    /// so it is a lower bound. Nothing in the data says which case you
    /// are in.
    ///
    /// Pairing does not rescue it, for the same reason pairing did not
    /// cancel corpus contamination: this is a system-by-label-
    /// completeness interaction, not a constant offset. An arm that
    /// ranks a correct-but-unjudged document first pushes the judged one
    /// past K and is recorded as a regression; an arm that overfits the
    /// miner's known subset looks better while retrieving fewer true
    /// answers.
    #[serde(default = "yes")]
    pub judgments_complete: bool,
    /// When true, the correct behaviour is to return nothing above the
    /// score floor. Must be paired with an empty `relevant` list.
    #[serde(default)]
    pub expect_abstention: bool,
    #[serde(default)]
    pub judgment_source: JudgmentSource,
    /// Cluster this query belongs to, when several were generated from a
    /// shared subject or template.
    ///
    /// Queries sharing a group are not independent observations. Any
    /// statistic computed over them must use the group as its sampling
    /// unit, or its confidence intervals will be too narrow by roughly
    /// the square root of the group size.
    #[serde(default)]
    pub group_id: Option<String>,
    /// Optional free-text note explaining why the judgment is what it is.
    #[serde(default)]
    pub note: Option<String>,
}

impl RecallQuery {
    /// Reject internally inconsistent queries at load time rather than
    /// letting them produce meaningless scores.
    pub fn validate(&self) -> Result<()> {
        if self.id.trim().is_empty() {
            bail!("query id must not be empty");
        }
        if self.text.trim().is_empty() {
            bail!("query {:?} has empty text", self.id);
        }
        if self.category.trim().is_empty() {
            bail!("query {:?} has empty category", self.id);
        }
        if self.expect_abstention && !self.relevant.is_empty() {
            bail!(
                "query {:?} expects abstention but lists {} relevant documents",
                self.id,
                self.relevant.len()
            );
        }
        if !self.expect_abstention && self.relevant.is_empty() {
            bail!(
                "query {:?} lists no relevant documents and is not marked expect_abstention; \
                 it would score zero for every strategy and only add noise",
                self.id
            );
        }
        for graded in &self.relevant {
            if graded.relevance > 3 {
                bail!(
                    "query {:?} judgment {:?} has relevance {} outside 0..=3",
                    self.id,
                    graded.key,
                    graded.relevance
                );
            }
            if graded.key.trim().is_empty() {
                bail!("query {:?} has an empty judgment key", self.id);
            }
        }
        let mut seen = std::collections::BTreeSet::new();
        for graded in &self.relevant {
            if !seen.insert(&graded.key) {
                bail!(
                    "query {:?} lists duplicate judgment key {:?}",
                    self.id,
                    graded.key
                );
            }
        }
        if let Some(waiver) = &self.capacity_waiver {
            if waiver.reason.trim().is_empty() {
                bail!(
                    "query {:?} declares a capacity waiver with no reason; an exception \
                     to the recall-ceiling gate has to say why the question genuinely \
                     has more than K right answers",
                    self.id
                );
            }
            if self.expect_abstention {
                bail!(
                    "query {:?} expects abstention and declares a capacity waiver; a \
                     query with no right answers cannot have too many",
                    self.id
                );
            }
        }
        for key in &self.exclude {
            if key.trim().is_empty() {
                bail!("query {:?} has an empty exclusion key", self.id);
            }
            // Excluding a document the same query calls relevant is a
            // contradiction, and it would silently zero that judgment.
            if seen.contains(key) {
                bail!(
                    "query {:?} both judges and excludes {:?}; a document cannot be \
                     the query's source and its answer",
                    self.id,
                    key
                );
            }
        }
        Ok(())
    }

    /// Keys with non-zero relevance.
    #[must_use]
    pub fn relevant_keys(&self) -> Vec<&str> {
        self.relevant
            .iter()
            .filter(|g| g.relevance > 0)
            .map(|g| g.key.as_str())
            .collect()
    }

    /// Relevance grade lookup for NDCG.
    #[must_use]
    pub fn grades(&self) -> BTreeMap<&str, u8> {
        self.relevant
            .iter()
            .map(|g| (g.key.as_str(), g.relevance))
            .collect()
    }
}

/// A loaded, validated set of evaluation queries.
#[derive(Debug, Clone, Default)]
pub struct RecallQuerySet {
    pub name: String,
    pub queries: Vec<RecallQuery>,
}

impl RecallQuerySet {
    /// Load a JSONL query set, validating every row and rejecting
    /// duplicate ids (which would double-count in the aggregate).
    pub fn from_jsonl(path: &Path, name: &str) -> Result<Self> {
        let body = std::fs::read_to_string(path)
            .with_context(|| format!("reading query set {}", path.display()))?;
        Self::from_str(&body, name)
    }

    /// Parse a JSONL query set from memory.
    pub fn from_str(body: &str, name: &str) -> Result<Self> {
        let mut queries = Vec::new();
        let mut ids = std::collections::BTreeSet::new();

        for (i, line) in body.lines().enumerate() {
            if line.trim().is_empty() || line.trim_start().starts_with('#') {
                continue;
            }
            let query: RecallQuery = serde_json::from_str(line)
                .with_context(|| format!("parsing query set line {}", i + 1))?;
            query
                .validate()
                .with_context(|| format!("validating query set line {}", i + 1))?;
            if !ids.insert(query.id.clone()) {
                bail!("duplicate query id {:?} on line {}", query.id, i + 1);
            }
            queries.push(query);
        }

        Ok(Self {
            name: name.to_string(),
            queries,
        })
    }

    /// Distinct categories present, in stable order.
    #[must_use]
    pub fn categories(&self) -> Vec<String> {
        let set: std::collections::BTreeSet<&str> =
            self.queries.iter().map(|q| q.category.as_str()).collect();
        set.into_iter().map(String::from).collect()
    }

    /// Count of queries whose correct answer is "nothing".
    #[must_use]
    pub fn abstention_count(&self) -> usize {
        self.queries.iter().filter(|q| q.expect_abstention).count()
    }
}

/// Reject a query whose text leaks its own answer.
///
/// A generated question that contains the target's exact path or a
/// unique symbol measures string matching, not retrieval. This is a
/// blunt guard used by generators, not a semantic judgement: it only
/// checks for verbatim containment of a key's distinctive tail.
#[must_use]
pub fn leaks_answer(query_text: &str, key: &str) -> bool {
    let haystack = query_text.to_ascii_lowercase();
    // Compare against the most specific parts of the key: its final path
    // segment (minus any chunk suffix) and the full path body.
    let body = key.rsplit('/').next().unwrap_or(key);
    let body = body.split('#').next().unwrap_or(body);
    let body = body.trim().to_ascii_lowercase();
    if body.len() < 4 {
        return false;
    }
    haystack.contains(&body)
}

#[cfg(test)]
mod tests {
    #[test]
    fn adjudicated_judgments_are_their_own_tier() {
        use super::JudgmentSource;

        // The shortcut this guards against is labelling model review as
        // human review. The provenance split exists so mechanically
        // mined judgments cannot quietly decide a design question; a tier
        // that collapses into "human-reviewed" would defeat it silently,
        // and the gates would then report a human slice that no human
        // ever looked at.
        assert_eq!(
            JudgmentSource::ModelAdjudicated.as_str(),
            "model-adjudicated"
        );
        assert_ne!(
            JudgmentSource::ModelAdjudicated.as_str(),
            JudgmentSource::HumanReviewed.as_str()
        );
        assert_ne!(
            JudgmentSource::ModelAdjudicated,
            JudgmentSource::HumanReviewed
        );

        // It does count as independently checked — that is the whole
        // point of having it — but only alongside, never instead of.
        assert!(JudgmentSource::ModelAdjudicated.is_independently_checked());
        assert!(JudgmentSource::HumanReviewed.is_independently_checked());
        assert!(!JudgmentSource::Generated.is_independently_checked());

        // And it must survive a round trip under its own name, so a
        // stored query set cannot be reloaded as something stronger.
        let json = serde_json::to_string(&JudgmentSource::ModelAdjudicated).unwrap();
        assert_eq!(json, "\"model-adjudicated\"");
        let back: JudgmentSource = serde_json::from_str(&json).unwrap();
        assert_eq!(back, JudgmentSource::ModelAdjudicated);
    }

    use super::*;

    fn q(json: &str) -> Result<RecallQuery> {
        let parsed: RecallQuery = serde_json::from_str(json)?;
        parsed.validate()?;
        Ok(parsed)
    }

    #[test]
    fn accepts_a_well_formed_query() {
        let parsed = q(
            r#"{"id":"q1","text":"how does decay work","category":"direct",
             "relevant":[{"key":"omem://a/code/x.rs#0","relevance":2}]}"#,
        )
        .unwrap();
        assert_eq!(parsed.relevant_keys(), vec!["omem://a/code/x.rs#0"]);
        assert_eq!(parsed.grades()["omem://a/code/x.rs#0"], 2);
        assert_eq!(parsed.judgment_source, JudgmentSource::Generated);
    }

    #[test]
    fn accepts_an_abstention_query() {
        let parsed = q(
            r#"{"id":"q2","text":"who won in 1817","category":"abstention",
             "expect_abstention":true}"#,
        )
        .unwrap();
        assert!(parsed.expect_abstention);
        assert!(parsed.relevant_keys().is_empty());
    }

    #[test]
    fn rejects_abstention_with_judgments() {
        let err = q(
            r#"{"id":"q","text":"t","category":"c","expect_abstention":true,
             "relevant":[{"key":"k"}]}"#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("expects abstention"), "{err}");
    }

    #[test]
    fn rejects_query_with_no_judgments_and_no_abstention_flag() {
        let err = q(r#"{"id":"q","text":"t","category":"c"}"#).unwrap_err();
        assert!(err.to_string().contains("no relevant documents"), "{err}");
    }

    #[test]
    fn rejects_out_of_range_relevance_and_duplicates() {
        let err = q(r#"{"id":"q","text":"t","category":"c",
             "relevant":[{"key":"k","relevance":9}]}"#)
        .unwrap_err();
        assert!(err.to_string().contains("outside 0..=3"), "{err}");

        let err = q(r#"{"id":"q","text":"t","category":"c",
             "relevant":[{"key":"k"},{"key":"k"}]}"#)
        .unwrap_err();
        assert!(err.to_string().contains("duplicate judgment key"), "{err}");
    }

    #[test]
    fn rejects_a_query_that_both_judges_and_excludes_a_document() {
        let err =
            q(r#"{"id":"q","text":"t","category":"c","relevant":[{"key":"k"}],"exclude":["k"]}"#)
                .unwrap_err();
        assert!(
            err.to_string().contains("both judges and excludes"),
            "{err}"
        );
    }

    #[test]
    fn accepts_an_exclusion_disjoint_from_the_judgments() {
        let parsed = q(
            r#"{"id":"q","text":"t","category":"commit-to-file","relevant":[{"key":"file"}],"exclude":["source"]}"#,
        )
        .unwrap();
        assert_eq!(parsed.exclude, vec!["source".to_string()]);
    }

    #[test]
    fn query_set_rejects_duplicate_ids() {
        let body = concat!(
            r#"{"id":"a","text":"t","category":"c","relevant":[{"key":"k"}]}"#,
            "\n",
            r#"{"id":"a","text":"t2","category":"c","relevant":[{"key":"k2"}]}"#,
        );
        let err = RecallQuerySet::from_str(body, "s").unwrap_err();
        assert!(err.to_string().contains("duplicate query id"), "{err}");
    }

    #[test]
    fn query_set_skips_blanks_and_comments() {
        let body = concat!(
            "# a comment\n",
            "\n",
            r#"{"id":"a","text":"t","category":"direct","relevant":[{"key":"k"}]}"#,
            "\n",
            r#"{"id":"b","text":"t","category":"abstention","expect_abstention":true}"#,
        );
        let set = RecallQuerySet::from_str(body, "s").unwrap();
        assert_eq!(set.queries.len(), 2);
        assert_eq!(set.categories(), vec!["abstention", "direct"]);
        assert_eq!(set.abstention_count(), 1);
    }

    #[test]
    fn leakage_guard_catches_verbatim_paths() {
        assert!(leaks_answer(
            "what does recall.rs do",
            "omem://a/code/crates/g/src/recall.rs#3"
        ));
        assert!(!leaks_answer(
            "how are retrieval scores decayed over time",
            "omem://a/code/crates/g/src/recall.rs#3"
        ));
    }

    #[test]
    fn leakage_guard_ignores_tiny_keys() {
        assert!(!leaks_answer("anything at all", "a/b#0"));
    }
}
