//! Shared conversation-selection behavior for GUI and TUI terminal surfaces.

use warp_core::features::FeatureFlag;
use warpui::{AppContext, Entity, EntityId, ModelContext, ModelHandle, SingletonEntity};

use super::agent_view::{
    AgentViewController, AgentViewControllerEvent, AgentViewDisplayMode, AgentViewEntryOrigin,
    EnterAgentViewError,
};
use super::{BlocklistAIHistoryEvent, BlocklistAIHistoryModel};
use crate::ai::agent::conversation::{
    AIConversation, AIConversationAutoexecuteMode, AIConversationId,
};

/// The conversation targeted by the next query from a terminal surface.
#[derive(Debug, Clone, Eq, PartialEq)]
pub enum PendingQueryState {
    /// The next query will continue an existing conversation.
    Existing { conversation_id: AIConversationId },
    New {
        /// Autoexecute override for the new conversation to be started.
        autoexecute_override: AIConversationAutoexecuteMode,
    },
}

impl Default for PendingQueryState {
    fn default() -> Self {
        Self::New {
            autoexecute_override: AIConversationAutoexecuteMode::default(),
        }
    }
}

impl PendingQueryState {
    /// Returns whether the next query targets an existing conversation.
    pub fn targets_existing_conversation(&self) -> bool {
        matches!(self, PendingQueryState::Existing { .. })
    }
}

enum ConversationSurface {
    TerminalView {
        agent_view_controller: ModelHandle<AgentViewController>,
    },
    Tui,
}

/// Events shared models use without depending on GUI Agent View state.
#[derive(Clone, Debug)]
pub enum ConversationSurfaceEvent {
    PendingQueryStateUpdated,
    AgentViewEntered {
        display_mode: AgentViewDisplayMode,
        origin: AgentViewEntryOrigin,
    },
    AgentViewExited {
        conversation_id: AIConversationId,
        final_exchange_count: usize,
        is_exit_before_new_entrance: bool,
    },
}

/// Per-terminal-surface selection and Agent View lifecycle boundary.
pub struct ConversationSurfaceModel {
    terminal_surface_id: EntityId,
    pending_query_state: PendingQueryState,
    surface: ConversationSurface,
}

impl ConversationSurfaceModel {
    /// Creates conversation state for a GUI terminal view.
    pub(crate) fn new_for_terminal_view(
        terminal_surface_id: EntityId,
        agent_view_controller: ModelHandle<AgentViewController>,
        ctx: &mut ModelContext<Self>,
    ) -> Self {
        ctx.subscribe_to_model(&agent_view_controller, |_, _, event, ctx| match event {
            AgentViewControllerEvent::EnteredAgentView {
                display_mode,
                origin,
                ..
            } => ctx.emit(ConversationSurfaceEvent::AgentViewEntered {
                display_mode: *display_mode,
                origin: origin.clone(),
            }),
            AgentViewControllerEvent::ExitedAgentView {
                conversation_id,
                final_exchange_count,
                is_exit_before_new_entrance,
                ..
            } => ctx.emit(ConversationSurfaceEvent::AgentViewExited {
                conversation_id: *conversation_id,
                final_exchange_count: *final_exchange_count,
                is_exit_before_new_entrance: *is_exit_before_new_entrance,
            }),
            AgentViewControllerEvent::ExitConfirmed { .. } => {}
        });

        Self::new(
            terminal_surface_id,
            ConversationSurface::TerminalView {
                agent_view_controller,
            },
            ctx,
        )
    }

    /// Creates conversation state for a TUI surface.
    #[cfg(feature = "tui")]
    pub(crate) fn new_for_tui_surface(
        terminal_surface_id: EntityId,
        ctx: &mut ModelContext<Self>,
    ) -> Self {
        Self::new(terminal_surface_id, ConversationSurface::Tui, ctx)
    }

    /// Creates conversation state without production subscriptions.
    #[cfg(test)]
    pub(crate) fn new_for_terminal_view_test(
        terminal_surface_id: EntityId,
        agent_view_controller: ModelHandle<AgentViewController>,
    ) -> Self {
        Self {
            terminal_surface_id,
            pending_query_state: PendingQueryState::default(),
            surface: ConversationSurface::TerminalView {
                agent_view_controller,
            },
        }
    }

    /// Creates TUI conversation state without production subscriptions.
    #[cfg(test)]
    pub(crate) fn new_for_tui_surface_test(terminal_surface_id: EntityId) -> Self {
        Self {
            terminal_surface_id,
            pending_query_state: PendingQueryState::default(),
            surface: ConversationSurface::Tui,
        }
    }

    /// Creates TUI conversation state with production history subscriptions for tests.
    #[cfg(test)]
    pub(crate) fn new_for_tui_surface_with_history_test(
        terminal_surface_id: EntityId,
        ctx: &mut ModelContext<Self>,
    ) -> Self {
        Self::new(terminal_surface_id, ConversationSurface::Tui, ctx)
    }

    fn new(
        terminal_surface_id: EntityId,
        surface: ConversationSurface,
        ctx: &mut ModelContext<Self>,
    ) -> Self {
        ctx.subscribe_to_model(
            &BlocklistAIHistoryModel::handle(ctx),
            |model, _, event, ctx| model.handle_history_event(event, ctx),
        );

        let pending_query_state =
            if warp_core::execution_mode::AppExecutionMode::as_ref(ctx).is_sandboxed() {
                PendingQueryState::New {
                    autoexecute_override: AIConversationAutoexecuteMode::RunToCompletion,
                }
            } else {
                PendingQueryState::default()
            };

        Self {
            terminal_surface_id,
            pending_query_state,
            surface,
        }
    }

    /// Returns whether this surface currently uses GUI Agent View selection.
    pub fn uses_agent_view_selection(&self) -> bool {
        FeatureFlag::AgentView.is_enabled()
            && matches!(self.surface, ConversationSurface::TerminalView { .. })
    }

    /// Returns whether this surface has an active GUI Agent View.
    pub fn is_agent_view_active(&self, app: &AppContext) -> bool {
        self.agent_view_controller()
            .is_some_and(|controller| controller.as_ref(app).is_active())
    }

    /// Returns whether this surface has a fullscreen GUI Agent View.
    pub fn is_agent_view_fullscreen(&self, app: &AppContext) -> bool {
        self.agent_view_controller()
            .is_some_and(|controller| controller.as_ref(app).is_fullscreen())
    }

    /// Returns the conversation targeted by the next query.
    pub fn selected_conversation_id(&self, app: &AppContext) -> Option<AIConversationId> {
        if self.uses_agent_view_selection() {
            return self.agent_view_controller().and_then(|controller| {
                controller
                    .as_ref(app)
                    .agent_view_state()
                    .active_conversation_id()
            });
        }

        match self.pending_query_state {
            PendingQueryState::Existing { conversation_id } => Some(conversation_id),
            PendingQueryState::New { .. } => None,
        }
    }

    /// Returns the selected conversation, if it is loaded.
    pub fn selected_conversation<'a>(&self, app: &'a AppContext) -> Option<&'a AIConversation> {
        self.selected_conversation_id(app)
            .as_ref()
            .and_then(|conversation_id| {
                BlocklistAIHistoryModel::as_ref(app).conversation(conversation_id)
            })
    }

    /// Returns the pending query state for this surface.
    pub fn pending_query_state(&self) -> &PendingQueryState {
        &self.pending_query_state
    }

    /// Selects an existing conversation for the next query.
    pub fn select_existing_conversation(
        &mut self,
        conversation_id: AIConversationId,
        origin: AgentViewEntryOrigin,
        ctx: &mut ModelContext<Self>,
    ) {
        self.set_pending_query_state(PendingQueryState::Existing { conversation_id }, ctx);
        if self.uses_agent_view_selection() {
            let Some(agent_view_controller) = self.agent_view_controller().cloned() else {
                return;
            };
            if let Err(error) = agent_view_controller.update(ctx, |controller, ctx| {
                controller.try_enter_agent_view(Some(conversation_id), origin, ctx)
            }) {
                log::error!("Failed to enter agent view for existing conversation: {error}");
            }
        }
    }

    /// Selects the new-conversation state for the next query.
    pub fn select_new_conversation(
        &mut self,
        origin: AgentViewEntryOrigin,
        ctx: &mut ModelContext<Self>,
    ) {
        self.set_pending_query_state(PendingQueryState::default(), ctx);
        if self.uses_agent_view_selection() {
            let Some(agent_view_controller) = self.agent_view_controller().cloned() else {
                return;
            };
            if let Err(error) = agent_view_controller.update(ctx, |controller, ctx| {
                controller.try_enter_agent_view(None, origin, ctx)
            }) {
                log::error!("Failed to enter agent view for new conversation: {error}");
            }
        }
    }

    /// Starts and selects a new conversation for this surface.
    pub fn try_start_new_conversation(
        &mut self,
        origin: AgentViewEntryOrigin,
        ctx: &mut ModelContext<Self>,
    ) -> Result<AIConversationId, EnterAgentViewError> {
        let (conversation_id, pending_query_state) = if self.uses_agent_view_selection() {
            let agent_view_controller = self
                .agent_view_controller()
                .cloned()
                .expect("Agent View selection requires a GUI controller");
            let conversation_id = agent_view_controller.update(ctx, |controller, ctx| {
                controller.try_enter_agent_view(None, origin, ctx)
            })?;
            (conversation_id, PendingQueryState::default())
        } else {
            let conversation_id =
                BlocklistAIHistoryModel::handle(ctx).update(ctx, |history, ctx| {
                    history.start_new_conversation(
                        self.terminal_surface_id,
                        false,
                        false,
                        false,
                        ctx,
                    )
                });
            (
                conversation_id,
                PendingQueryState::Existing { conversation_id },
            )
        };
        self.set_pending_query_state(pending_query_state, ctx);
        Ok(conversation_id)
    }

    /// Returns the autoexecute override for the pending query.
    pub fn pending_query_autoexecute_override(
        &self,
        app: &AppContext,
    ) -> AIConversationAutoexecuteMode {
        match &self.pending_query_state {
            PendingQueryState::New {
                autoexecute_override,
            } => *autoexecute_override,
            PendingQueryState::Existing { conversation_id } => BlocklistAIHistoryModel::as_ref(app)
                .conversation(conversation_id)
                .map(|conversation| conversation.autoexecute_override())
                .unwrap_or_default(),
        }
    }

    /// Toggles the autoexecute override for the pending query.
    pub fn toggle_pending_query_autoexecute(&mut self, ctx: &mut ModelContext<Self>) {
        if self.uses_agent_view_selection() {
            if let Some(conversation_id) = self.selected_conversation_id(ctx) {
                BlocklistAIHistoryModel::handle(ctx).update(ctx, |history, ctx| {
                    history.toggle_autoexecute_override(
                        &conversation_id,
                        self.terminal_surface_id,
                        ctx,
                    );
                });
            }
            return;
        }

        match &mut self.pending_query_state {
            PendingQueryState::New {
                autoexecute_override,
            } => {
                *autoexecute_override = if *autoexecute_override
                    == AIConversationAutoexecuteMode::RespectUserSettings
                {
                    AIConversationAutoexecuteMode::RunToCompletion
                } else {
                    AIConversationAutoexecuteMode::RespectUserSettings
                };
                ctx.emit(ConversationSurfaceEvent::PendingQueryStateUpdated);
            }
            PendingQueryState::Existing { conversation_id } => {
                BlocklistAIHistoryModel::handle(ctx).update(ctx, |history, ctx| {
                    history.toggle_autoexecute_override(
                        conversation_id,
                        self.terminal_surface_id,
                        ctx,
                    );
                });
            }
        }
    }

    /// Returns whether the pending query targets an existing conversation.
    pub fn is_targeting_existing_conversation(&self) -> bool {
        self.pending_query_state.targets_existing_conversation()
    }

    fn agent_view_controller(&self) -> Option<&ModelHandle<AgentViewController>> {
        match &self.surface {
            ConversationSurface::TerminalView {
                agent_view_controller,
            } => Some(agent_view_controller),
            ConversationSurface::Tui => None,
        }
    }

    fn set_pending_query_state(&mut self, state: PendingQueryState, ctx: &mut ModelContext<Self>) {
        if self.pending_query_state != state {
            self.pending_query_state = state;
            ctx.emit(ConversationSurfaceEvent::PendingQueryStateUpdated);
        }
    }

    fn handle_history_event(
        &mut self,
        event: &BlocklistAIHistoryEvent,
        ctx: &mut ModelContext<Self>,
    ) {
        if event
            .terminal_surface_id()
            .is_some_and(|id| id != self.terminal_surface_id)
        {
            return;
        }

        match event {
            BlocklistAIHistoryEvent::ClearedConversationsForTerminalSurface { .. } => {
                self.set_pending_query_state(PendingQueryState::default(), ctx);
                if self.uses_agent_view_selection() {
                    if let Some(agent_view_controller) = self.agent_view_controller().cloned() {
                        agent_view_controller
                            .update(ctx, |controller, ctx| controller.exit_agent_view(ctx));
                    }
                }
            }
            BlocklistAIHistoryEvent::SplitConversation {
                old_conversation_id,
                new_conversation_id,
                ..
            } => {
                if self.selected_conversation_id(ctx) == Some(*old_conversation_id) {
                    self.select_existing_conversation(
                        *new_conversation_id,
                        AgentViewEntryOrigin::AgentRequestedNewConversation,
                        ctx,
                    );
                }
            }
            BlocklistAIHistoryEvent::RemoveConversation {
                conversation_id, ..
            }
            | BlocklistAIHistoryEvent::DeletedConversation {
                conversation_id, ..
            }
            | BlocklistAIHistoryEvent::ConversationTransferredBetweenTerminalSurfaces {
                conversation_id,
                ..
            } => {
                if !self.uses_agent_view_selection()
                    && self.selected_conversation_id(ctx) == Some(*conversation_id)
                {
                    self.set_pending_query_state(PendingQueryState::default(), ctx);
                }
            }
            _ => {}
        }
    }
}

impl Entity for ConversationSurfaceModel {
    type Event = ConversationSurfaceEvent;
}

#[cfg(test)]
#[path = "conversation_surface_model_tests.rs"]
mod tests;
