//! Take detection: lines the speaker recorded more than once.
//!
//! Pure functions on a media transcript (`filmcraft_project::Transcript`): split the words into
//! utterances, compare neighbouring utterances by normalised-word similarity and spoken retake
//! cues, and bundle similar passes into `TakeGroup`s (media time). The engine (`takes.*`
//! commands) assigns group ids, stores the groups on the transcript and switches takes on the
//! timeline. See `openspec/changes/take-editing/design.md` §4.
