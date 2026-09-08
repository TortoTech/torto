use rebook_layout::{LayoutViewport, ReaderStyle};
use rebook_publication::{Book, LocatorV1};
use rebook_reader::{
    NavigationAttempt, NavigationPreparation, NavigationResult, NavigationToken, PageDirection,
    PreparedNavigation, ReaderError, ReaderPosition, ReaderSession, ReaderSnapshot, ReaderSpread,
};

pub struct EngineReader {
    session: ReaderSession,
}

impl EngineReader {
    pub(crate) fn new(session: ReaderSession) -> Self {
        Self { session }
    }

    pub fn book(&self) -> &Book {
        self.session.book()
    }

    pub fn snapshot(&self) -> ReaderSnapshot {
        self.session.snapshot()
    }

    pub fn current_spread(&mut self) -> Result<ReaderSpread, ReaderError> {
        self.session.current_spread()
    }

    pub fn spread_at(&mut self, position: ReaderPosition) -> Result<ReaderSpread, ReaderError> {
        self.session.spread_at(position)
    }

    pub fn current_position(&self) -> ReaderPosition {
        self.session.current_position()
    }

    pub fn current_spread_positions(&mut self) -> Result<Vec<ReaderPosition>, ReaderError> {
        self.session.current_spread_positions()
    }

    pub fn current_locator(&self) -> LocatorV1 {
        self.session.current_locator()
    }

    pub fn prefetch_adjacent(&mut self) -> Result<(), ReaderError> {
        self.session.prefetch_adjacent()
    }

    pub fn try_turn_page(
        &mut self,
        direction: PageDirection,
    ) -> Result<NavigationAttempt, ReaderError> {
        self.session.try_turn_page(direction)
    }

    pub fn resize(&mut self, viewport: LayoutViewport) -> Result<ReaderSnapshot, ReaderError> {
        self.session.resize(viewport)
    }

    pub fn set_style(&mut self, style: ReaderStyle) -> Result<ReaderSnapshot, ReaderError> {
        self.session.set_style(style)
    }

    pub fn prepare_navigation(
        &mut self,
        direction: PageDirection,
    ) -> Result<NavigationPreparation, ReaderError> {
        self.session.prepare_navigation(direction)
    }

    pub fn poll_navigation(
        &mut self,
        token: NavigationToken,
    ) -> Result<NavigationPreparation, ReaderError> {
        self.session.poll_navigation(token)
    }

    pub fn commit_navigation(
        &mut self,
        prepared: PreparedNavigation,
    ) -> Result<NavigationResult, ReaderError> {
        self.session.commit_navigation(prepared)
    }

    pub fn cancel_navigation(&mut self, token: NavigationToken) -> bool {
        self.session.cancel_navigation(token)
    }

    pub fn session(&self) -> &ReaderSession {
        &self.session
    }

    pub fn session_mut(&mut self) -> &mut ReaderSession {
        &mut self.session
    }
}
