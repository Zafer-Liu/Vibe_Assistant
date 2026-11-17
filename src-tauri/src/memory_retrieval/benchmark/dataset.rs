//! Bounded, local-only benchmark dataset parsing. Diagnostics never echo payload text.
use super::types::{Dataset, DatasetSummary};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};

const MAX_JSON_BYTES: usize = 10 * 1024 * 1024;

pub(crate) fn valid_id(id: &str) -> bool {
    (1..=80).contains(&id.len())
        && id.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
}

pub(crate) fn parse_and_validate(text: &str) -> Result<Dataset, String> {
    if text.len() > MAX_JSON_BYTES {
        return Err("dataset/json_size: exceeds 10 MiB".into());
    }
    // serde diagnostics may contain input values; expose only position and category.
    let dataset = serde_json::from_str(text).map_err(|e| {
        format!("dataset/json: invalid schema at line {}, column {}", e.line(), e.column())
    })?;
    validate(&dataset)?;
    Ok(dataset)
}

fn require(ok: bool, scope: &str, field: &str) -> Result<(), String> {
    if ok { Ok(()) } else { Err(format!("{scope}/{field}: invalid or inconsistent value")) }
}

fn reference_id(id: &str) -> bool {
    !id.trim().is_empty() && id.chars().count() <= 512 && !id.chars().any(char::is_control)
}

fn references(ids: &[String], known: &HashSet<&str>) -> bool {
    let mut seen = HashSet::new();
    ids.iter().all(|id| known.contains(id.as_str()) && seen.insert(id.as_str()))
}

pub(crate) fn validate(dataset: &Dataset) -> Result<(), String> {
    require(dataset.schema_version == 1, "dataset", "schema_version")?;
    require(valid_id(&dataset.dataset_id), "dataset", "dataset_id")?;
    require(!dataset.name.trim().is_empty() && dataset.name.chars().count() <= 256, "dataset", "name")?;
    require(dataset.description.chars().count() <= 4000, "dataset", "description")?;
    require((1..=32).contains(&dataset.domains.len()), "dataset", "domains_count")?;
    require((1..=512).contains(&dataset.memories.len()), "dataset", "memories_count")?;
    require((1..=200).contains(&dataset.cases.len()), "dataset", "cases_count")?;
    let mut domain_ids = HashSet::new();
    for domain in &dataset.domains {
        require(reference_id(&domain.id) && domain_ids.insert(domain.id.as_str()), "domain", "id")?;
        require(!domain.name.trim().is_empty() && domain.name.chars().count() <= 256, "domain", "name")?;
        require(domain.description.chars().count() <= 4000, "domain", "description")?;
    }
    require(dataset.sources.len() <= 128, "dataset", "sources_count")?;
    let mut source_texts = HashMap::new();
    for source in &dataset.sources {
        require(reference_id(&source.event_key) && !source_texts.contains_key(source.event_key.as_str()), "source", "event_key")?;
        require(reference_id(&source.source_agent), "source", "source_agent")?;
        require(reference_id(&source.session_id), "source", "session_id")?;
        require(chrono::DateTime::parse_from_rfc3339(&source.occurred_at).is_ok(), "source", "occurred_at")?;
        require(source.text.chars().count() <= 1024 * 1024, "source", "text_size")?;
        source_texts.insert(source.event_key.as_str(), source.text.as_str());
    }
    let mut memory_ids = HashSet::new();
    let mut sources = HashMap::new();
    for memory in &dataset.memories {
        require(reference_id(&memory.id) && memory_ids.insert(memory.id.as_str()), "memory", "id")?;
        require(!memory.content.trim().is_empty() && memory.content.chars().count() <= 32_000, "memory", "content")?;
        require(references(&memory.domains, &domain_ids), "memory", "domains")?;
        require(!memory.memory_type.trim().is_empty() && memory.memory_type.chars().count() <= 64, "memory", "memory_type")?;
        require(matches!(memory.durability.as_str(), "session" | "short_term" | "long_term"), "memory", "durability")?;
        require(reference_id(&memory.source_event_key), "memory", "source_event_key")?;
        require(reference_id(&memory.source_agent), "memory", "source_agent")?;
        require(reference_id(&memory.session_id), "memory", "session_id")?;
        require(memory.origin_device.as_deref().map_or(true, reference_id), "memory", "origin_device")?;
        let mut keys = HashSet::from([memory.source_event_key.as_str()]);
        let mut provenance_ids = HashSet::new();
        for provenance in &memory.provenance {
            require(reference_id(&provenance.source_id) && provenance_ids.insert(provenance.source_id.as_str()), "memory", "provenance_source_id")?;
            require(reference_id(&provenance.source_event_key), "memory", "provenance_source_event_key")?;
            require(reference_id(&provenance.source_agent), "memory", "provenance_source_agent")?;
            require(reference_id(&provenance.session_id), "memory", "provenance_session_id")?;
            keys.insert(provenance.source_event_key.as_str());
        }
        sources.insert(memory.id.as_str(), keys);
    }
    let mut case_ids = HashSet::new();
    for case in &dataset.cases {
        // Only validated, bounded ASCII identifiers may appear in diagnostics.
        require(valid_id(&case.case_id), "case", "case_id")?;
        let scope = format!("case {}", case.case_id);
        require(case_ids.insert(case.case_id.as_str()), &scope, "case_id")?;
        require(!case.category.trim().is_empty(), &scope, "category")?;
        require(matches!(case.split.as_str(), "development" | "holdout"), &scope, "split")?;
        require(!case.query.trim().is_empty() && case.query.chars().count() <= 4096, &scope, "query")?;
        require((1..=50).contains(&case.limit), &scope, "limit")?;
        require(matches!(case.domain_mode.as_str(), "auto" | "manual" | "off"), &scope, "domain_mode")?;
        require(case.domains.len() <= 2 && references(&case.domains, &domain_ids), &scope, "domains")?;
        require(case.domain_mode != "manual" || !case.domains.is_empty(), &scope, "manual_domains")?;
        require(case.domain_mode != "off" || case.domains.is_empty(), &scope, "off_domains")?;
        require(references(&case.expected_ids, &memory_ids), &scope, "expected_ids")?;
        require(references(&case.forbidden_ids, &memory_ids), &scope, "forbidden_ids")?;
        require(!case.expected_ids.iter().any(|id| case.forbidden_ids.contains(id)), &scope, "expected_forbidden_overlap")?;
        require(case.answerable == !case.expected_ids.is_empty(), &scope, "answerable")?;
        // Expected routing may use the full domain set, independently of the two-domain request cap.
        require(references(&case.expected_domain_ids, &domain_ids), &scope, "expected_domain_ids")?;
        let expected_sources: HashSet<_> = case.expected_ids.iter()
            .filter_map(|id| sources.get(id.as_str())).flat_map(|keys| keys.iter().copied()).collect();
        require(references(&case.expected_source_event_keys, &expected_sources), &scope, "expected_source_event_keys")?;
        let mut annotations = HashSet::new();
        for evidence in &case.expected_evidence {
            require(!evidence.contains.trim().is_empty()
                && expected_sources.contains(evidence.source_event_key.as_str())
                && source_texts.get(evidence.source_event_key.as_str()).is_some_and(|text| text.contains(&evidence.contains))
                && annotations.insert((evidence.source_event_key.as_str(), evidence.contains.as_str())),
                &scope, "expected_evidence")?;
        }
        require(case.expected_evidence_status.as_deref().map_or(true, |status| {
            !case.expected_ids.is_empty() && (1..=80).contains(&status.len())
                && status.bytes().all(|c| c.is_ascii_lowercase() || c == b'_')
        }), &scope, "expected_evidence_status")?;
    }
    let bytes = serde_json::to_vec(dataset).map_err(|_| "dataset/json: serialization failed".to_string())?;
    require(bytes.len() <= MAX_JSON_BYTES, "dataset", "json_size")
}

pub(crate) fn summary(dataset: &Dataset) -> DatasetSummary {
    DatasetSummary {
        dataset_id: dataset.dataset_id.clone(), name: dataset.name.clone(),
        description: dataset.description.clone(), synthetic: dataset.synthetic,
        memory_count: dataset.memories.len(), case_count: dataset.cases.len(),
        development_count: dataset.cases.iter().filter(|c| c.split == "development").count(),
        holdout_count: dataset.cases.iter().filter(|c| c.split == "holdout").count(),
    }
}

/// SHA256 of compact DTO JSON; whitespace in imported JSON does not affect identity.
pub(crate) fn fingerprint(dataset: &Dataset) -> String {
    // DTO contains only JSON-serializable fields; serialization is infallible here.
    let bytes = serde_json::to_vec(dataset).expect("dataset DTO serialization");
    format!("{:x}", Sha256::digest(bytes))
}

pub(crate) fn builtin() -> Result<Dataset, String> {
    parse_and_validate(include_str!("example.json"))
}

pub(crate) fn builtin_evidence() -> Result<Dataset, String> {
    parse_and_validate(include_str!("evidence-example.json"))
}

pub(crate) fn is_builtin(id: &str) -> bool {
    matches!(id, "example-memory-v1" | "evidence-example-v1")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_is_a_labeled_pilot_with_fixed_splits() {
        let data = builtin().unwrap();
        let counts = summary(&data);
        assert_eq!(data.dataset_id, "example-memory-v1");
        assert!(data.synthetic);
        assert_eq!((counts.case_count, counts.development_count, counts.holdout_count), (40, 32, 8));
        assert_eq!(data.domains.len(), 4);
        assert!((16..=24).contains(&data.memories.len()));
        assert!(data.cases.iter().filter(|c| !c.answerable).count() >= 4);
        assert_eq!(fingerprint(&data).len(), 64);
    }

    #[test]
    fn evidence_builtin_and_legacy_empty_fields_are_compatible() {
        let evidence = builtin_evidence().unwrap();
        assert!(evidence.synthetic);
        assert_eq!((evidence.memories.len(), evidence.sources.len(), evidence.cases.len()), (8, 3, 8));
        assert!(is_builtin(&evidence.dataset_id));
        let legacy = serde_json::to_value(builtin().unwrap()).unwrap();
        assert!(legacy.get("sources").is_none());
        for memory in legacy["memories"].as_array().unwrap() {
            for field in ["user_defined", "origin_device", "provenance"] { assert!(memory.get(field).is_none()); }
        }
        for case in legacy["cases"].as_array().unwrap() {
            assert!(case.get("expected_evidence").is_none());
            assert!(case.get("expected_evidence_status").is_none());
        }
    }

    #[test]
    fn evidence_annotations_require_real_related_source_text() {
        let mut data = builtin_evidence().unwrap();
        data.cases[0].expected_evidence[0].contains = "private-nonexistent-quote".into();
        let error = validate(&data).unwrap_err();
        assert!(error.contains("expected_evidence"));
        assert!(!error.contains("private-nonexistent-quote"));
        data.cases[0].expected_evidence[0] = data.cases[7].expected_evidence[0].clone();
        assert!(validate(&data).is_err(), "an existing quote from an unrelated source is invalid");
        let mut data = builtin_evidence().unwrap();
        data.sources.push(data.sources[0].clone());
        assert!(validate(&data).is_err(), "duplicate source keys are rejected");
        let mut data = builtin_evidence().unwrap();
        data.sources[0].text = "界".repeat(1024 * 1024);
        data.cases[0].expected_evidence.clear();
        data.cases[1].expected_evidence.retain(|e| e.source_event_key != "synthetic-source-a");
        assert!(validate(&data).is_ok(), "source limit counts Unicode characters, not bytes");
        data.sources[0].text.push('界');
        assert!(validate(&data).is_err());
    }

    #[test]
    fn invalid_references_and_sensitive_payloads_are_not_echoed() {
        let mut data = builtin().unwrap();
        data.cases[0].query = "private-secret".into();
        data.cases[0].expected_source_event_keys = vec!["private-secret".into()];
        let error = validate(&data).unwrap_err();
        assert!(error.contains("expected_source_event_keys"));
        assert!(!error.contains("private-secret"));
        assert!(!parse_and_validate("{\"private-secret\":0}").err().unwrap().contains("private-secret"));
    }

    #[test]
    fn production_memory_ids_and_multi_domain_membership_are_valid() {
        let mut data = builtin().unwrap();
        let old = data.memories[0].id.clone();
        let replacement = "local-l1:source-event:0".to_string();
        data.memories[0].id = replacement.clone();
        data.memories[0].domains = data.domains.iter().map(|domain| domain.id.clone()).collect();
        for case in &mut data.cases {
            for id in case.expected_ids.iter_mut().chain(case.forbidden_ids.iter_mut()) {
                if *id == old { *id = replacement.clone(); }
            }
        }
        assert!(validate(&data).is_ok());
        assert!(!valid_id(&replacement), "dataset filenames retain their stricter ID rule");
    }

    #[test]
    fn expected_domains_can_cover_all_actual_domains() {
        let mut data = builtin().unwrap();
        data.cases[0].expected_domain_ids = data.domains.iter().map(|d| d.id.clone()).collect();
        assert!(validate(&data).is_ok());
        data.cases[0].domains = data.cases[0].expected_domain_ids.clone();
        assert!(validate(&data).is_err());
    }
}
