//! The one mechanism behind every operation the host carries out for a step (select the input, add the output, apply
//! the character, run the calibration). The machine never assumes an operation worked: it asks, remembers the request
//! by number, and acts only on that request's own answer.

/// Hands out request numbers. They are never reused within a sitting, so an answer can only find its own request.
#[derive(Clone, Debug, Default)]
pub(crate) struct Ids(u32);

impl Ids {
    fn next(&mut self) -> u32 {
        let id = self.0;
        self.0 = self.0.wrapping_add(1);
        id
    }
}

/// How an answer left the operation.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum Reply {
    /// The host applied the request's value.
    Applied,
    /// The host refused the value the person still wants: the step stays and the person tries again.
    Failed,
    /// The host refused a value the person has since moved away from, so nothing is lost.
    Superseded,
}

/// One kind of host operation. `T` is what is applied (a source, an output, a card); what the person currently wants
/// is held by the caller and passed in, so a later choice never has to wait for an earlier request to be answered.
#[derive(Clone, Debug)]
pub(crate) struct Op<T> {
    applied: Option<T>,
    flight: Option<(u32, T)>,
    failed: bool,
}

impl<T: Copy + PartialEq> Op<T> {
    /// An operation whose host state is `applied` (the host's own setting, if it has one).
    pub(crate) fn new(applied: Option<T>) -> Self {
        Op { applied, flight: None, failed: false }
    }

    /// What the host has applied.
    pub(crate) fn applied(&self) -> Option<T> {
        self.applied
    }

    /// The outstanding request, if any (the invariant test answers it).
    #[cfg(test)]
    pub(crate) fn request(&self) -> Option<u32> {
        self.flight.map(|(id, _)| id)
    }

    /// Whether `request` is this operation's outstanding one.
    pub(crate) fn owns(&self, request: u32) -> bool {
        self.flight.is_some_and(|(id, _)| id == request)
    }

    /// True when the host holds exactly what the person wants and nothing is outstanding.
    pub(crate) fn settled(&self, want: Option<T>) -> bool {
        want.is_some() && self.flight.is_none() && self.applied == want
    }

    /// True when the last request for what the person wants was refused and the person has not tried again.
    pub(crate) fn failed(&self, want: Option<T>) -> bool {
        self.failed && !self.settled(want)
    }

    /// Asks for `want` unless the host already holds it, a request is outstanding (it is followed up when answered)
    /// or the last one failed and the person has not asked again.
    pub(crate) fn ask(&mut self, want: Option<T>, ids: &mut Ids) -> Option<(u32, T)> {
        let value = want?;
        if self.flight.is_some() || self.settled(want) {
            return None;
        }
        let request = ids.next();
        self.flight = Some((request, value));
        self.failed = false;
        Some((request, value))
    }

    /// Takes the answer to the outstanding request (the caller checked [`Self::owns`]).
    pub(crate) fn reply(&mut self, ok: bool, want: Option<T>) -> Option<Reply> {
        let (_, value) = self.flight.take()?;
        if ok {
            self.applied = Some(value);
            Some(Reply::Applied)
        } else if want == Some(value) {
            self.failed = true;
            Some(Reply::Failed)
        } else {
            Some(Reply::Superseded)
        }
    }

    /// Forgets the outstanding request and any failure; a late answer to it is then ignored.
    pub(crate) fn cancel(&mut self) {
        self.flight = None;
        self.failed = false;
    }

    /// Cancels, and forgets what the host held too (it no longer applies to what the person wants).
    pub(crate) fn forget(&mut self) {
        self.cancel();
        self.applied = None;
    }
}
