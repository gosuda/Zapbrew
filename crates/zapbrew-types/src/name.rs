//! Formula names and their optional tap qualification.

use std::fmt;
use std::str::FromStr;

use crate::TypeError;

/// A formula reference: an unqualified `NAME` or a tap-qualified
/// `USER/REPO/NAME`.
///
/// The name charset is `HOMEBREW_TAP_FORMULA_NAME_REGEX` from
/// `.references/brew/Library/Homebrew/tap_constants.rb` — one or more of
/// `[A-Za-z0-9_+\-.@]` (`\w` is ASCII in Ruby's regex). The qualified form
/// follows `HOMEBREW_TAP_FORMULA_REGEX`: exactly three slash-separated
/// segments, where `USER` and `REPO` are non-empty and slash-free (any other
/// characters are allowed, faithfully to the regex), and `NAME` obeys the
/// name charset.
///
/// The original reference is stored once; [`FormulaName::name`] and
/// [`FormulaName::tap`] are views into it, so no parsed strings are
/// duplicated and accessors allocate nothing.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FormulaName {
    input: String,
    /// Byte offset of the name segment within `input`.
    name_off: usize,
    /// Byte offset just past the user segment, when tap-qualified.
    user_off: Option<usize>,
}

impl FormulaName {
    /// The formula name segment, without any tap qualification.
    pub fn name(&self) -> &str {
        &self.input[self.name_off..]
    }

    /// The qualifying tap as `(user, repository)`, or `None` when the
    /// reference is unqualified.
    pub fn tap(&self) -> Option<(&str, &str)> {
        let user_off = self.user_off?;
        Some((
            &self.input[..user_off - 1],
            &self.input[user_off..self.name_off - 1],
        ))
    }

    /// The full original reference (`NAME` or `USER/REPO/NAME`).
    pub fn as_str(&self) -> &str {
        &self.input
    }
}

impl AsRef<str> for FormulaName {
    fn as_ref(&self) -> &str {
        &self.input
    }
}

impl fmt::Display for FormulaName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.input)
    }
}

fn is_name_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '_' | '+' | '-' | '.' | '@')
}

impl FromStr for FormulaName {
    type Err = TypeError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let err = || TypeError::InvalidFormulaName(s.to_owned());

        let mut parts = s.split('/');
        let first = parts.next().unwrap_or("");
        let second = parts.next();
        let third = parts.next();
        let extra = parts.next();

        let (name_seg, name_off, user_off) = match (second, third, extra) {
            (None, None, None) => (first, 0, None),
            (Some(repo), Some(name), None) if !first.is_empty() && !repo.is_empty() => {
                let user_off = first.len() + 1;
                (name, user_off + repo.len() + 1, Some(user_off))
            }
            _ => return Err(err()),
        };

        if name_seg.is_empty() || !name_seg.chars().all(is_name_char) {
            return Err(err());
        }

        Ok(FormulaName {
            input: s.to_owned(),
            name_off,
            user_off,
        })
    }
}
