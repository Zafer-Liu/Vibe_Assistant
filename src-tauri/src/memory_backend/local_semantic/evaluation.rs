//! Evaluation uses the real embedding protocol and search path, but a caller-owned
//! cache. It never opens the production vector file or starts a configured process.
use super::*;
use crate::telemetry_store::LocalMemorySnapshot;
use std::path::Path;

pub(crate) struct EmbeddingPlan {
    pub model: String,
    pub remote: bool,
    pub batches: usize,
}

pub(crate) fn plan(value: Value, documents: &[LocalMemorySnapshot]) -> Result<EmbeddingPlan, String> {
    let config = Config::parse(value)?;
    let mut seen = HashSet::new();
    let inputs: Vec<_> = documents.iter().filter(|row| seen.insert(row.memory.as_str()))
        .map(|row| row.memory.as_str()).collect();
    Ok(EmbeddingPlan { model: config.model.clone(), remote: config.is_remote(), batches: plan_batches(&config, &inputs).len() })
}

pub(crate) fn prepare(
    value: Value, documents: &[LocalMemorySnapshot], path: &Path,
    budget: &RecallBudget, mut progress: impl FnMut(usize),
) -> Result<(), String> {
    budget.remaining()?;
    let config = Config::parse(value)?;
    // `path` is generated under this run's private directory, never user input.
    let mut conn = Connection::open(path).map_err(|_| "evaluation cache unavailable".to_string())?;
    initialize_cache(&conn).map_err(|_| "evaluation cache initialization failed".to_string())?;
    let signatures: Vec<_> = documents.iter().map(|row| signature(&config, &row.memory)).collect();
    let mut vectors = HashMap::new();
    let mut dimension = None;
    let missing = load_cached(&conn, &signatures, &(0..documents.len()).collect::<Vec<_>>(), Some(&mut vectors), &mut dimension)?;
    let inputs: Vec<_> = missing.iter().map(|&index| documents[index].memory.as_str()).collect();
    let batches = plan_batches(&config, &inputs);
    progress(signatures.iter().filter(|key| vectors.contains_key(*key)).count());
    for batch in batches {
        budget.remaining()?;
        let timeout = budget.remaining()?.min(Duration::from_secs(120));
        let client = reqwest::blocking::Client::builder().no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(timeout.min(Duration::from_secs(15))).timeout(timeout)
            .build().map_err(|_| "evaluation embedding client unavailable".to_string())?;
        let texts: Vec<_> = batch.iter().map(|&offset| inputs[offset]).collect();
        let embedded = embed_batch(&client, &config, &config.request_headers()?, &texts, dimension)
            .map_err(|_| "evaluation document embedding failed".to_string())?;
        budget.remaining()?;
        if dimension.is_none() { dimension = embedded.first().map(Vec::len); }
        let transaction = conn.transaction().map_err(|_| "evaluation cache transaction failed".to_string())?;
        for (&offset, vector) in batch.iter().zip(embedded) {
            budget.remaining()?;
            let key = &signatures[missing[offset]];
            let bytes: Vec<_> = vector.iter().flat_map(|value| value.to_le_bytes()).collect();
            transaction.execute("INSERT OR REPLACE INTO embeddings_v1(signature,dimension,vector) VALUES (?1,?2,?3)",
                params![key, vector.len(), bytes]).map_err(|_| "evaluation cache write failed".to_string())?;
            vectors.insert(key.clone(), vector);
        }
        transaction.commit().map_err(|_| "evaluation cache commit failed".to_string())?;
        progress(signatures.iter().filter(|key| vectors.contains_key(*key)).count());
    }
    budget.remaining()?;
    if signatures.iter().any(|key| !vectors.contains_key(key)) {
        return Err("evaluation index is incomplete".into());
    }
    progress(documents.len());
    Ok(())
}

pub(crate) fn search(
    store: &TelemetryStore, value: Value, path: &Path, query: &str, limit: u32,
    domains: Option<&[String]>, budget: &RecallBudget,
) -> Result<crate::memory_backend::SemanticRecall, String> {
    search_interactive_at(store, value, query, limit, domains, budget, Some(path))
}
