//! Reusable per-surface TUI conversation coordination.

use anyhow::anyhow;
use warpui::{AppContext, Entity, EntityId, ModelContext, ModelHandle, SingletonEntity};

use crate::ai::agent::conversation::{AIConversationId, ConversationStatus};
use crate::ai::blocklist::agent_view::AgentViewEntryOrigin;
use crate::ai::blocklist::{
    BlocklistAIController, BlocklistAIHistoryEvent, BlocklistAIHistoryModel,
    ConversationStatusUpdate, ConversationSurfaceEvent, ConversationSurfaceModel,
};

/// Events emitted by a TUI conversation model for presentation layers.
#[derive(Clone, Debug)]
pub(super) enum TuiConversationModelEvent {
    SelectedConversationChanged {
        conversation_id: Option<AIConversationId>,
    },
    ConversationStarted {
        conversation_id: AIConversationId,
    },
    ConversationUpdated {
        conversation_id: AIConversationId,
    },
    ConversationStatusChanged {
        conversation_id: AIConversationId,
        status: ConversationStatus,
        update: ConversationStatusUpdate,
    },
    Error {
        message: String,
    },
}

/// Per-surface conversation/composer model for a future interactive TUI.
///
/// This model deliberately contains no transcript widgets. It coordinates selected
/// conversation state, conversation restore/create operations, prompt
/// submission, and history-backed stream events for one TUI surface.
pub(super) struct TuiConversationModel {
    terminal_surface_id: EntityId,
    conversation_surface: ModelHandle<ConversationSurfaceModel>,
    ai_controller: ModelHandle<BlocklistAIController>,
}

impl TuiConversationModel {
    /// Creates a TUI conversation model around the shared production AI models.
    pub(super) fn new(
        terminal_surface_id: EntityId,
        conversation_surface: ModelHandle<ConversationSurfaceModel>,
        ai_controller: ModelHandle<BlocklistAIController>,
        ctx: &mut ModelContext<Self>,
    ) -> Self {
        ctx.subscribe_to_model(&conversation_surface, |model, _, event, ctx| {
            if matches!(event, ConversationSurfaceEvent::PendingQueryStateUpdated) {
                ctx.emit(TuiConversationModelEvent::SelectedConversationChanged {
                    conversation_id: model.selected_conversation_id(ctx),
                });
            }
        });
        ctx.subscribe_to_model(
            &BlocklistAIHistoryModel::handle(ctx),
            |model, _, event, ctx| model.handle_history_event(event, ctx),
        );
        Self {
            terminal_surface_id,
            conversation_surface,
            ai_controller,
        }
    }

    /// Returns this surface's currently selected next-prompt target.
    fn selected_conversation_id(&self, ctx: &AppContext) -> Option<AIConversationId> {
        self.conversation_surface
            .as_ref(ctx)
            .selected_conversation_id(ctx)
    }

    /// Selects a live conversation as this surface's next-prompt target.
    fn select_conversation(
        &mut self,
        conversation_id: AIConversationId,
        ctx: &mut ModelContext<Self>,
    ) -> anyhow::Result<()> {
        let is_live = BlocklistAIHistoryModel::as_ref(ctx)
            .all_live_conversations_for_terminal_surface(self.terminal_surface_id)
            .any(|conversation| conversation.id() == conversation_id);
        if !is_live {
            return Err(anyhow!(
                "Conversation {conversation_id} is not live for TUI surface {}",
                self.terminal_surface_id
            ));
        }
        self.conversation_surface.update(ctx, |surface, ctx| {
            surface.select_existing_conversation(conversation_id, AgentViewEntryOrigin::Cli, ctx);
        });
        Ok(())
    }

    /// Creates and selects an empty conversation for this TUI surface.
    fn start_new_conversation(
        &mut self,
        ctx: &mut ModelContext<Self>,
    ) -> anyhow::Result<AIConversationId> {
        self.conversation_surface
            .update(ctx, |surface, ctx| {
                surface.try_start_new_conversation(AgentViewEntryOrigin::Cli, ctx)
            })
            .map_err(Into::into)
    }

    /// Sends a prompt to this surface's selected conversation, creating one if needed.
    pub(super) fn send_prompt(&mut self, prompt: String, ctx: &mut ModelContext<Self>) {
        let conversation_id = match self.selected_conversation_id(ctx) {
            Some(conversation_id) => conversation_id,
            None => match self.start_new_conversation(ctx) {
                Ok(conversation_id) => conversation_id,
                Err(error) => {
                    self.emit_error(error, ctx);
                    return;
                }
            },
        };
        self.ai_controller.update(ctx, |controller, ctx| {
            controller.send_user_query_in_conversation(prompt, conversation_id, None, ctx);
        });
    }

    /// Restores, selects, and sends a prompt to an existing conversation.
    pub(super) fn restore_conversation_and_send_prompt(
        &mut self,
        prompt: String,
        conversation_id: AIConversationId,
        ctx: &mut ModelContext<Self>,
    ) {
        let history = BlocklistAIHistoryModel::handle(ctx);
        let is_live = history
            .as_ref(ctx)
            .all_live_conversations_for_terminal_surface(self.terminal_surface_id)
            .any(|conversation| conversation.id() == conversation_id);
        if is_live {
            if let Err(error) = self.select_conversation(conversation_id, ctx) {
                self.emit_error(error, ctx);
                return;
            }
            self.send_prompt(prompt, ctx);
            return;
        }
        if let Some(conversation) = history.as_ref(ctx).conversation(&conversation_id).cloned() {
            history.update(ctx, |history, ctx| {
                history.restore_conversations(self.terminal_surface_id, vec![conversation], ctx);
            });
            if let Err(error) = self.select_conversation(conversation_id, ctx) {
                self.emit_error(error, ctx);
                return;
            }
            self.send_prompt(prompt, ctx);
            return;
        }

        let future = history
            .as_ref(ctx)
            .load_conversation_data(conversation_id, ctx);
        ctx.spawn(future, move |model, conversation, ctx| {
            let Some(crate::ai::blocklist::history_model::CloudConversationData::Oz(conversation)) =
                conversation
            else {
                model.emit_error(
                    anyhow!("Failed to load local conversation {conversation_id}"),
                    ctx,
                );
                return;
            };
            BlocklistAIHistoryModel::handle(ctx).update(ctx, |history, ctx| {
                history.restore_conversations(model.terminal_surface_id, vec![*conversation], ctx);
            });
            if let Err(error) = model.select_conversation(conversation_id, ctx) {
                model.emit_error(error, ctx);
                return;
            }
            model.send_prompt(prompt, ctx);
        });
    }

    /// Converts terminal-surface-scoped history events into TUI presentation events.
    fn handle_history_event(
        &mut self,
        event: &BlocklistAIHistoryEvent,
        ctx: &mut ModelContext<Self>,
    ) {
        if event
            .terminal_surface_id()
            .is_some_and(|terminal_surface_id| terminal_surface_id != self.terminal_surface_id)
        {
            return;
        }
        match event {
            BlocklistAIHistoryEvent::StartedNewConversation {
                new_conversation_id,
                ..
            } => ctx.emit(TuiConversationModelEvent::ConversationStarted {
                conversation_id: *new_conversation_id,
            }),
            BlocklistAIHistoryEvent::UpdatedStreamingExchange {
                conversation_id, ..
            } => ctx.emit(TuiConversationModelEvent::ConversationUpdated {
                conversation_id: *conversation_id,
            }),
            BlocklistAIHistoryEvent::UpdatedConversationStatus {
                conversation_id,
                new_status,
                update,
                ..
            } => ctx.emit(TuiConversationModelEvent::ConversationStatusChanged {
                conversation_id: *conversation_id,
                status: new_status.clone(),
                update: update.clone(),
            }),
            _ => {}
        }
    }

    /// Emits a presentation-safe model error.
    fn emit_error(&self, error: anyhow::Error, ctx: &mut ModelContext<Self>) {
        ctx.emit(TuiConversationModelEvent::Error {
            message: format!("{error:#}"),
        });
    }
}

impl Entity for TuiConversationModel {
    type Event = TuiConversationModelEvent;
}
