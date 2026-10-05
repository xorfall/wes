use crate::{
    Call, Diagnostic,
    vocabulary::{CommandSpec, MetaCommand, commands},
};
use std::sync::Arc;
use wes_core::capability::{Capability, Catalogue, ProviderDescription};

#[derive(Clone, Debug)]
pub enum Resolution {
    Meta {
        call: Call,
        spec: CommandSpec,
        tail: Vec<String>,
        target: Option<crate::targets::QueryTarget>,
    },
    Capability {
        call: Call,
        provider: Arc<ProviderDescription>,
        capability: Arc<Capability>,
    },
}
impl Resolution {
    pub fn call(&self) -> &Call {
        match self {
            Self::Meta { call, .. } | Self::Capability { call, .. } => call,
        }
    }
}

/// Resolution consumes metadata only. It cannot call a provider or consult credentials.
pub fn resolve(call: &Call, catalogue: &Catalogue) -> Result<Resolution, Diagnostic> {
    let Some(head) = call.path.first() else {
        return Err(
            Diagnostic::error("RES004", call.span, "a command needs a name")
                .with_public_message("Missing command name."),
        );
    };
    let meta = crate::vocabulary::commands::is_command_root(&head.text);
    let provider = catalogue.provider(&head.text);
    let tail = call
        .path
        .iter()
        .skip(1)
        .map(|name| name.text.clone())
        .collect::<Vec<_>>();
    if call.marker.is_some() {
        return if meta {
            as_meta(call, catalogue)
        } else {
            Err(suggest(
                Diagnostic::error(
                    "RES002",
                    head.span,
                    format!("there is no meta command called '{}'", head.text),
                )
                .with_public_message("Unknown meta command."),
                &head.text,
                commands::roots(),
            ))
        };
    }
    match (meta, provider) {
        (true, Some(_)) => Err(Diagnostic::error(
            "RES001",
            head.span,
            format!(
                "'{}' is both a meta command and an imported provider",
                head.text
            ),
        ).with_public_message("Command name is ambiguous between a provider and a meta command. Use the colon for meta commands.")
        .with_hint(format!("write ':{}' for the meta command", head.text))
        .with_hint(format!(
            "re-import the provider under another name, as in ':import … as {}2'",
            head.text
        ))),
        (true, None) => as_meta(call, catalogue),
        (false, Some(provider)) => match provider.capability(&tail) {
            Some(capability) => Ok(Resolution::Capability {
                call: call.clone(),
                provider: provider.clone(),
                capability: capability.clone(),
            }),
            None => Err(suggest(
                Diagnostic::error(
                    "RES005",
                    call.span,
                    format!("'{}' offers nothing called '{}'", head.text, tail.join(" ")),
                ).with_public_message("Capability path is absent from this provider."),
                &tail.join(" "),
                provider
                    .capabilities()
                    .map(|capability| capability.path.join(" ")),
            )
            .with_hint(format!(
                "':list capabilities provider:{}' shows what it offers",
                head.text
            ))),
        },
        (false, None) => Err(suggest(
            Diagnostic::error(
                "RES004",
                head.span,
                format!(
                    "'{}' is neither a meta command nor an imported provider",
                    head.text
                ),
            ).with_public_message("Provider or command is absent from the selected catalogue."),
            &head.text,
            commands::roots().into_iter()
                .chain(catalogue.provider_names()),
        )
        .with_hint("':list providers' shows what is imported")),
    }
}

fn as_meta(call: &Call, catalogue: &Catalogue) -> Result<Resolution, Diagnostic> {
    let crate::vocabulary::commands::CommandInvocation {
        mut call,
        mut spec,
        mut tail,
    } = crate::vocabulary::commands::invocation(call)?;
    let target = if matches!(spec.command, MetaCommand::Help | MetaCommand::Inspect) {
        let target = crate::targets::resolve(&call, spec.command == MetaCommand::Help, catalogue)?;
        // The typed target carries the selector; only output references participate in binding.
        call.arguments.clear();
        call.operands.clear();
        if let crate::targets::QueryTarget::Output { reference, .. } = &target {
            call.operands
                .push(crate::Value::Reference(reference.clone()));
        }
        tail.clear();
        call.path.truncate(1);
        spec.requires_subject = false;
        spec.path_tail = crate::vocabulary::Arity::NONE;
        spec.tail_words.clear();
        Some(target)
    } else {
        None
    };
    Ok(Resolution::Meta {
        call,
        spec,
        tail,
        target,
    })
}

fn suggest(
    diagnostic: Diagnostic,
    written: &str,
    candidates: impl IntoIterator<Item = impl AsRef<str>>,
) -> Diagnostic {
    if let Some(suggestion) = closest(written, candidates) {
        diagnostic.with_hint(format!("did you mean '{suggestion}'?"))
    } else {
        diagnostic
    }
}

pub(crate) fn closest(
    written: &str,
    candidates: impl IntoIterator<Item = impl AsRef<str>>,
) -> Option<String> {
    let mut best = None;
    let mut distance = 3;
    for candidate in candidates {
        let candidate = candidate.as_ref();
        let next = bounded_distance(written, candidate, 2);
        if next < distance {
            distance = next;
            best = Some(candidate.to_string());
        }
    }
    best
}

/// UTF-16 edit distance, banded to the only useful suggestion range. Long input stays bounded.
fn bounded_distance(left: &str, right: &str, limit: usize) -> usize {
    let left = left.encode_utf16().collect::<Vec<_>>();
    let right = right.encode_utf16().collect::<Vec<_>>();
    if left.len().abs_diff(right.len()) > limit {
        return limit + 1;
    }
    let mut previous = (0..=right.len()).collect::<Vec<_>>();
    let mut current = vec![limit + 1; right.len() + 1];
    for row in 1..=left.len() {
        current[0] = row;
        let start = row.saturating_sub(limit).max(1);
        let end = right.len().min(row + limit);
        if start > 1 {
            current[start - 1] = limit + 1;
        }
        if end < right.len() {
            current[end + 1] = limit + 1;
        }
        let mut minimum = current[0];
        for column in start..=end {
            current[column] = (previous[column - 1]
                + usize::from(left[row - 1] != right[column - 1]))
            .min(previous[column] + 1)
            .min(current[column - 1] + 1);
            minimum = minimum.min(current[column]);
        }
        if minimum > limit {
            return limit + 1;
        }
        std::mem::swap(&mut previous, &mut current);
    }
    previous[right.len()]
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bounded_suggestions_match_full_distance_for_exhaustive_small_inputs() {
        let mut words = vec![String::new()];
        for size in 1..=6 {
            for bits in 0..(1 << size) {
                words.push(
                    (0..size)
                        .map(|bit| if bits & (1 << bit) == 0 { 'a' } else { 'b' })
                        .collect(),
                );
            }
        }
        fn full(a: &str, b: &str) -> usize {
            let b = b.as_bytes();
            let mut row = (0..=b.len()).collect::<Vec<_>>();
            for (i, a) in a.bytes().enumerate() {
                let mut diagonal = row[0];
                row[0] = i + 1;
                for j in 1..=b.len() {
                    let old = row[j];
                    row[j] = (diagonal + usize::from(a != b[j - 1]))
                        .min(row[j] + 1)
                        .min(row[j - 1] + 1);
                    diagonal = old;
                }
            }
            row[b.len()]
        }
        for a in &words {
            for b in &words {
                assert_eq!(
                    bounded_distance(a, b, 2).min(3),
                    full(a, b).min(3),
                    "{a:?} {b:?}"
                );
            }
        }
        assert_eq!(
            bounded_distance(&"a".repeat(100_000), &"a".repeat(100_000), 2),
            0
        );
    }
}
