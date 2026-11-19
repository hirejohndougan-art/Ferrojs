//! Test262 front-matter parsing (the YAML between `/*---` and `---*/`).
//!
//! Follows Test262's INTERPRETING.md. Unknown keys are ignored; malformed
//! metadata is an error, because silently mis-reading a test would make the
//! conformance numbers untrustworthy.

use yaml_rust2::{Yaml, YamlLoader};

/// When an expected error must be thrown.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Phase {
    /// While parsing or checking early errors.
    Parse,
    /// During module resolution.
    Resolution,
    /// During evaluation.
    Runtime,
}

impl Phase {
    fn parse(s: &str) -> Option<Phase> {
        match s {
            "parse" => Some(Phase::Parse),
            "resolution" => Some(Phase::Resolution),
            "runtime" => Some(Phase::Runtime),
            _ => None,
        }
    }
}

/// The `negative` attribute: the test must throw this error in this phase.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Negative {
    pub phase: Phase,
    pub error_type: String,
}

/// The `flags` attribute.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Flags {
    pub only_strict: bool,
    pub no_strict: bool,
    pub module: bool,
    pub raw: bool,
    pub is_async: bool,
    pub can_block_is_false: bool,
    pub can_block_is_true: bool,
    pub non_deterministic: bool,
}

/// Everything the runner needs from a test file's metadata.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Metadata {
    pub includes: Vec<String>,
    pub flags: Flags,
    pub negative: Option<Negative>,
    pub features: Vec<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum MetadataError {
    Missing,
    Unterminated,
    Yaml(String),
    Invalid(String),
}

impl std::fmt::Display for MetadataError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MetadataError::Missing => write!(f, "no /*--- metadata block"),
            MetadataError::Unterminated => write!(f, "metadata block has no closing ---*/"),
            MetadataError::Yaml(e) => write!(f, "metadata is not valid YAML: {e}"),
            MetadataError::Invalid(e) => write!(f, "invalid metadata: {e}"),
        }
    }
}

fn string_list(value: &Yaml, key: &str) -> Result<Vec<String>, MetadataError> {
    match value {
        Yaml::BadValue | Yaml::Null => Ok(Vec::new()),
        Yaml::Array(items) => items
            .iter()
            .map(|item| {
                item.as_str().map(str::to_owned).ok_or_else(|| {
                    MetadataError::Invalid(format!("`{key}` must be a list of strings"))
                })
            })
            .collect(),
        _ => Err(MetadataError::Invalid(format!("`{key}` must be a list"))),
    }
}

/// Extracts and parses the metadata block from a test file's source text.
pub fn parse(source: &str) -> Result<Metadata, MetadataError> {
    let start = source.find("/*---").ok_or(MetadataError::Missing)?;
    let body_start = start + "/*---".len();
    let end = source[body_start..]
        .find("---*/")
        .ok_or(MetadataError::Unterminated)?;
    let yaml_text = &source[body_start..body_start + end];

    let docs =
        YamlLoader::load_from_str(yaml_text).map_err(|e| MetadataError::Yaml(e.to_string()))?;
    let doc = match docs.into_iter().next() {
        Some(doc) => doc,
        None => return Ok(Metadata::default()),
    };

    let mut meta = Metadata {
        includes: string_list(&doc["includes"], "includes")?,
        features: string_list(&doc["features"], "features")?,
        ..Metadata::default()
    };

    for flag in string_list(&doc["flags"], "flags")? {
        match flag.as_str() {
            "onlyStrict" => meta.flags.only_strict = true,
            "noStrict" => meta.flags.no_strict = true,
            "module" => meta.flags.module = true,
            "raw" => meta.flags.raw = true,
            "async" => meta.flags.is_async = true,
            "CanBlockIsFalse" => meta.flags.can_block_is_false = true,
            "CanBlockIsTrue" => meta.flags.can_block_is_true = true,
            "non-deterministic" => meta.flags.non_deterministic = true,
            // Informational only; no effect on how the test runs.
            "generated" => {}
            other => return Err(MetadataError::Invalid(format!("unknown flag `{other}`"))),
        }
    }

    let negative = &doc["negative"];
    if !negative.is_badvalue() && !negative.is_null() {
        let phase = negative["phase"]
            .as_str()
            .and_then(Phase::parse)
            .ok_or_else(|| {
                MetadataError::Invalid("negative.phase must be parse, resolution or runtime".into())
            })?;
        let error_type = negative["type"]
            .as_str()
            .ok_or_else(|| MetadataError::Invalid("negative.type must be a string".into()))?
            .to_owned();
        meta.negative = Some(Negative { phase, error_type });
    }

    Ok(meta)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_full_block() {
        let src = "// header\n/*---\nesid: sec-x\ndescription: >\n  folded\n  text\nincludes: [compareArray.js, propertyHelper.js]\nflags: [onlyStrict, async, generated]\nfeatures:\n  - Array.prototype.at\n  - Symbol\nnegative:\n  phase: parse\n  type: SyntaxError\n---*/\nbody();\n";
        let m = parse(src).unwrap();
        assert_eq!(m.includes, ["compareArray.js", "propertyHelper.js"]);
        assert_eq!(m.features, ["Array.prototype.at", "Symbol"]);
        assert!(m.flags.only_strict && m.flags.is_async && !m.flags.module);
        assert_eq!(
            m.negative,
            Some(Negative {
                phase: Phase::Parse,
                error_type: "SyntaxError".into()
            })
        );
    }

    #[test]
    fn empty_lists_and_missing_keys() {
        let m = parse("/*---\ndescription: x\n---*/").unwrap();
        assert_eq!(m, Metadata::default());
    }

    #[test]
    fn rejects_bad_metadata() {
        assert_eq!(parse("no block"), Err(MetadataError::Missing));
        assert_eq!(
            parse("/*--- description: x"),
            Err(MetadataError::Unterminated)
        );
        assert!(matches!(
            parse("/*---\nflags: [bogus]\n---*/"),
            Err(MetadataError::Invalid(_))
        ));
        assert!(matches!(
            parse("/*---\nnegative:\n  phase: later\n  type: SyntaxError\n---*/"),
            Err(MetadataError::Invalid(_))
        ));
    }
}
