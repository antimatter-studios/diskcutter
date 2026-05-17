//! `identify_data_stream` — the recursion engine of the chain.
//!
//! Given a source reader, walks the static `READERS` registry asking
//! each format "is this you?". The first format that claims the source
//! wraps it; the wrapped reader is then re-fed through this same
//! function so layered compressions (e.g. `.iso.xz.gz`) peel cleanly.
//!
//! Termination: when no registered format claims the source, the
//! source is returned unchanged along with the accumulated chain of
//! labels (innermost first, e.g. `["xz", "raw"]` for an xz file).
//!
//! Recursion depth is bounded by `MAX_DEPTH` to defend against
//! pathological inputs (e.g. a malicious file that looks like
//! `gz(gz(gz(…)))` forever). 16 layers is generous — real-world stacks
//! are 1–2.

use super::format::FormatTryOpen;
use super::interface::ReaderInterface;
use crate::joblog::JobLogger;
use std::io;

/// Safety cap on chain depth. 16 is generous: real-world stacks are
/// 1-2 (raw, or `.img.xz`), occasional 3 (`.img.xz.gz` for testing).
const MAX_DEPTH: usize = 16;

/// Peel decoder layers off `src` until no registered format matches.
/// Returns the final (innermost-yielding-raw-bytes) reader and the
/// chain of labels traversed.
///
/// `labels[0]` is the outermost format that matched, `labels[last]`
/// is always `"raw"` — appended unconditionally so callers can rely
/// on a non-empty chain ending in the raw-bytes producer.
///
/// `log` receives one debug entry per layer attempt and one info entry
/// summarising the final chain. Pass `&NullLogger` for probe/inspect
/// callers not attached to a queue item.
pub fn identify_data_stream(
    mut src: Box<dyn ReaderInterface>,
    registry: &[&'static dyn FormatTryOpen],
    log: &dyn JobLogger,
) -> io::Result<(Box<dyn ReaderInterface>, Vec<&'static str>)> {
    let mut labels: Vec<&'static str> = Vec::new();
    for depth in 0..MAX_DEPTH {
        if log.debug_enabled() {
            log.debug(&format!(
                "decoder_chain: identify depth={depth} trying {n} formats",
                n = registry.len()
            ));
        }
        match try_one_round(src, registry, log) {
            Ok((wrapped, label)) => {
                if log.debug_enabled() {
                    log.debug(&format!("decoder_chain: matched layer {depth} = {label}"));
                }
                labels.push(label);
                src = wrapped;
            }
            Err(unchanged) => {
                if log.debug_enabled() {
                    log.debug(&format!(
                        "decoder_chain: no format claimed depth={depth}, terminating at raw"
                    ));
                }
                labels.push("raw");
                log.info(&format!("decoder_chain: {}", labels.join(" → ")));
                return Ok((unchanged, labels));
            }
        }
    }
    log.error(&format!(
        "decoder_chain: exceeded MAX_DEPTH ({MAX_DEPTH}) — possible recursive format bomb"
    ));
    Err(io::Error::new(
        io::ErrorKind::InvalidData,
        format!("decoder chain exceeded MAX_DEPTH ({MAX_DEPTH}) — possible recursive format bomb"),
    ))
}

/// Result of one `try_one_round` step: `Ok` carries the wrapped reader
/// and its label; `Err` carries the rewound source for the next attempt.
type RoundResult = Result<(Box<dyn ReaderInterface>, &'static str), Box<dyn ReaderInterface>>;

/// One iteration of the identify loop. Tries each format in order;
/// returns `Ok((wrapped, label))` on the first match, `Err(src)`
/// with the rewound source if nothing matched.
fn try_one_round(
    mut src: Box<dyn ReaderInterface>,
    registry: &[&'static dyn FormatTryOpen],
    log: &dyn JobLogger,
) -> RoundResult {
    for fmt in registry {
        match fmt.try_open(src) {
            Ok(wrapped) => return Ok((wrapped, fmt.label())),
            Err(returned) => {
                if log.debug_enabled() {
                    log.debug(&format!(
                        "decoder_chain: format {label} declined",
                        label = fmt.label()
                    ));
                }
                src = returned;
            }
        }
    }
    Err(src)
}
