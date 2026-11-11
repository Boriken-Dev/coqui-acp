use std::collections::HashMap;

use crate::Error;

const MAX_IDS: usize = 4096;

/// The request ids this host made, kept for as long as a binding to one can verify, so that an id is not used twice and an answer
/// is taken once.
#[derive(Debug, Default)]
pub(crate) struct Ledger {
    /// id to (when it may be forgotten, whether it was answered).
    ids: HashMap<String, (u64, bool)>,
}

impl Ledger {
    /// Notes a new id. Ids past their memory are forgotten first.
    pub(crate) fn admit(&mut self, id: &str, now: u64, memory: u64) -> Result<(), Error> {
        self.ids.retain(|_, (forget, _)| *forget > now);
        if self.ids.contains_key(id) {
            return Err(Error::Replayed);
        }
        if self.ids.len() >= MAX_IDS {
            return Err(Error::Limit("too many requests in the memory window"));
        }
        self.ids
            .insert(id.to_owned(), (now.saturating_add(memory), false));
        Ok(())
    }

    /// Marks an id answered. An id that was never admitted, was forgotten or was answered already is refused.
    pub(crate) fn complete(&mut self, id: &str) -> Result<(), Error> {
        match self.ids.get_mut(id) {
            Some((_, answered @ false)) => {
                *answered = true;
                Ok(())
            }
            _ => Err(Error::Replayed),
        }
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.ids.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_id_is_used_once_and_answered_once_within_the_memory() {
        let mut ledger = Ledger::default();
        ledger.admit("a", 100, 330).unwrap();
        assert_eq!(ledger.admit("a", 200, 330), Err(Error::Replayed));
        ledger.complete("a").unwrap();
        assert_eq!(ledger.complete("a"), Err(Error::Replayed));
        assert_eq!(ledger.complete("never"), Err(Error::Replayed));
        // Past the memory it is forgotten (a binding that old no longer verifies).
        ledger.admit("b", 431, 330).unwrap();
        assert_eq!(ledger.len(), 1);
        assert_eq!(ledger.complete("a"), Err(Error::Replayed));
    }
}
