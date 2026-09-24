//! Synthesizes CommandEntry rows from an agent's cached config_options.
//! Vendor-neutral; calls into spur-acp's config-option synthesizers. Also
//! owns [`ModelEffortCatalog`], the single grok / kiro / standard-
//! config-options precedence implementation every frontend advertises
//! `/model` and `/effort` through (spec 2026-09-23 §3.5).

use std::collections::{BTreeMap, BTreeSet};
use std::ops::Deref;

use spur_acp::adapter::arg_picker_hint::{ArgPickerChoice, ArgPickerHint, ArgPickerSpec};
use spur_acp::adapter::config_options::{synthesize, synthesize_advertised, AdvertisedCommand};
use spur_acp::adapter::grok_session_display::GrokSessionDisplay;
use spur_acp::adapter::kiro_session_display::KiroSessionDisplay;
use spur_acp::capability_evidence::{
    CapabilityChoice, CapabilityKind, DispatchRoute, EvidenceEpochId, EvidenceProvenance,
    ReducedCapability,
};
use spur_acp::{SessionConfigOption, SpurAgentCaps};

use crate::entry::{CommandEntry, CommandSource, Dispatch};

/// One selectable value in a [`ModelEffortCatalog`] plane (spec
/// 2026-09-23 §3.5). Field-compatible with the notebook's
/// `ChatConfigChoice` so its P2 mapping is a field-for-field copy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogChoice {
    /// Wire value (e.g. `modelId`, config-option select value).
    pub value: String,
    /// Human-readable picker label.
    pub label: String,
    /// Optional description from the agent catalog.
    pub description: Option<String>,
}

/// The neutral model / reasoning-effort catalog synthesized from an
/// agent's caps (spec 2026-09-23 §3.5) — the single grok / kiro /
/// standard-config-options precedence implementation shared by every
/// frontend. Frontends map this type to their own picker models; the
/// `/model` and `/effort` [`CommandEntry`] mapping stays in this crate.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ModelEffortCatalog {
    /// Advertised models, in agent order.
    pub models: Vec<CatalogChoice>,
    /// Currently selected model id, when advertised.
    pub current_model: Option<String>,
    /// Advertised efforts, scoped to the current model where the vendor
    /// catalog is model-scoped (Grok).
    pub efforts: Vec<CatalogChoice>,
    /// Currently selected effort id, when advertised.
    pub current_effort: Option<String>,
}

impl ModelEffortCatalog {
    /// Synthesize the catalog from a capability snapshot.
    ///
    /// Precedence: the standard config-option plane wins whenever it
    /// advertises a model or effort select; otherwise Grok's proprietary
    /// catalog, then Kiro's recovered models plane. An agent never has more
    /// than one vendor plane, and both extractors are agent-kind gated, so
    /// the vendor fallbacks are mutually exclusive in practice.
    #[must_use]
    pub fn from_caps(caps: &SpurAgentCaps) -> Self {
        Self::vendor_from_caps(caps)
            .unwrap_or_else(|| Self::from_config_options(&caps.config_options))
    }

    /// The vendor planes only — `None` when the standard config-option
    /// plane won the precedence or no vendor plane exists. The guard is
    /// [`Self::from_config_options`] itself, so [`Self::from_caps`] and
    /// the `CommandEntry` synthesis below share one precedence rule and
    /// the direct `SetSessionModel` / `SetSessionEffort` entries never
    /// double-emit over the standard entries from `synthesize_advertised`.
    pub(crate) fn vendor_from_caps(caps: &SpurAgentCaps) -> Option<Self> {
        let standard = Self::from_config_options(&caps.config_options);
        if !standard.models.is_empty() || !standard.efforts.is_empty() {
            return None;
        }
        caps.grok_display
            .as_ref()
            .map(Self::from_grok_display)
            .or_else(|| caps.kiro_display.as_ref().map(Self::from_kiro_display))
    }

    fn from_config_options(options: &[SessionConfigOption]) -> Self {
        let mut catalog = Self::default();
        for command in synthesize(options) {
            let choices = command
                .choices
                .into_iter()
                .map(|choice| CatalogChoice {
                    value: choice.value,
                    label: choice.label,
                    description: choice.description,
                })
                .collect();
            match command.name.as_str() {
                "model" => {
                    catalog.models = choices;
                    catalog.current_model = command.current_value;
                }
                "effort" => {
                    catalog.efforts = choices;
                    catalog.current_effort = command.current_value;
                }
                _ => {}
            }
        }
        catalog
    }

    fn from_grok_display(display: &GrokSessionDisplay) -> Self {
        let current_model = display.model_id.clone();
        let efforts = current_model
            .as_deref()
            .map(|model_id| {
                display
                    .efforts_for_model(model_id)
                    .iter()
                    .map(|effort| CatalogChoice {
                        value: effort.id.clone(),
                        label: effort.label.clone(),
                        description: None,
                    })
                    .collect()
            })
            .unwrap_or_default();
        Self {
            models: display
                .models()
                .iter()
                .map(|model| CatalogChoice {
                    value: model.id.clone(),
                    label: model.label.clone(),
                    description: None,
                })
                .collect(),
            current_model,
            efforts,
            current_effort: display.effort_id.clone(),
        }
    }

    fn from_kiro_display(display: &KiroSessionDisplay) -> Self {
        Self {
            models: display
                .models()
                .iter()
                .map(|model| CatalogChoice {
                    value: model.id.clone(),
                    label: model.label.clone(),
                    description: model.description.clone(),
                })
                .collect(),
            current_model: display.model_id.clone(),
            efforts: Vec::new(),
            current_effort: None,
        }
    }
}

pub struct AdvertisedSource;

/// The reduced route and immutable evidence epoch selected for one normalized
/// slash-command name. Public because the TUI submit router consults
/// `pinned_route_for_command` when reducing capability routes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PinnedCapabilityRoute {
    pub evidence_epoch: EvidenceEpochId,
    pub route: DispatchRoute,
}

/// Visible advertised entries plus the reduced route decision for every
/// capability name, including names reduced to `Hidden`.
#[derive(Debug, Clone, Default)]
pub struct AdvertisedEntries {
    entries: Vec<CommandEntry>,
    routes: BTreeMap<String, PinnedCapabilityRoute>,
    /// Standard ACP commands that remain authoritative while the evidence
    /// capture is incomplete. These names resolve same-handle collisions but
    /// deliberately carry no evidence-derived route or epoch pin.
    protocol_authority: BTreeSet<String>,
    /// Capability names observed in an incomplete evidence epoch. These
    /// names suppress same-handle vendor candidates without asserting which
    /// dispatch route the unfinished epoch would have selected.
    incomplete_evidence_names: BTreeSet<String>,
    /// Prompt command names decoded from complete command notifications
    /// before the evidence epoch became incomplete. These may retain an
    /// existing dynamic `PromptText` entry without pinning a route.
    incomplete_prompt_names: BTreeSet<String>,
}

type AdvertisedEntryParts = (
    Vec<CommandEntry>,
    BTreeMap<String, PinnedCapabilityRoute>,
    BTreeSet<String>,
    BTreeSet<String>,
    BTreeSet<String>,
);

impl AdvertisedEntries {
    pub(crate) fn into_parts(self) -> AdvertisedEntryParts {
        (
            self.entries,
            self.routes,
            self.protocol_authority,
            self.incomplete_evidence_names,
            self.incomplete_prompt_names,
        )
    }
}

impl From<Vec<CommandEntry>> for AdvertisedEntries {
    fn from(entries: Vec<CommandEntry>) -> Self {
        Self {
            entries,
            routes: BTreeMap::new(),
            protocol_authority: BTreeSet::new(),
            incomplete_evidence_names: BTreeSet::new(),
            incomplete_prompt_names: BTreeSet::new(),
        }
    }
}

impl Deref for AdvertisedEntries {
    type Target = [CommandEntry];

    fn deref(&self) -> &Self::Target {
        &self.entries
    }
}

impl IntoIterator for AdvertisedEntries {
    type Item = CommandEntry;
    type IntoIter = std::vec::IntoIter<CommandEntry>;

    fn into_iter(self) -> Self::IntoIter {
        self.entries.into_iter()
    }
}

#[derive(Debug, Clone)]
struct ReducedCommand {
    kind: CapabilityKind,
    upstream_id: String,
    choices: BTreeMap<String, CapabilityChoice>,
    pinned: PinnedCapabilityRoute,
}

impl AdvertisedSource {
    /// Build `CommandEntry` rows from a per-session capability snapshot.
    pub fn entries_from_caps(handle: &str, caps: &SpurAgentCaps) -> AdvertisedEntries {
        let mut candidates = synthesize_advertised(caps)
            .into_iter()
            .map(|adv: AdvertisedCommand| CommandEntry {
                name: adv.name,
                description: adv.description,
                hint: adv.hint,
                source: CommandSource::Advertised {
                    handle: handle.to_owned(),
                },
                dispatch: Dispatch::SetSessionConfigOption {
                    config_id: adv.config_id,
                },
                arg_picker_spec: Some(adv.arg_picker_spec),
            })
            .collect::<Vec<_>>();
        candidates.extend(mode_entries(handle, caps));

        // A partial raw frame makes the evidence epoch inconclusive, but it
        // does not invalidate config options or modes already decoded from a
        // complete ACP response. Keep those protocol-backed commands usable
        // without assigning an evidence-derived route. Vendor-specific native
        // candidates remain excluded until the capture becomes complete.
        if let Some(snapshot) = caps
            .capability_evidence
            .as_ref()
            .filter(|snapshot| !snapshot.is_complete())
        {
            let entries = deduplicate_legacy_candidates(candidates);
            let protocol_authority = entries
                .iter()
                .map(|entry| normalize_command_name(&entry.name))
                .collect();
            let config_names = synthesize_advertised(caps)
                .into_iter()
                .map(|command| (command.config_id, command.name))
                .collect::<BTreeMap<_, _>>();
            let incomplete_evidence_names = snapshot
                .reduced_capabilities()
                .iter()
                .flat_map(|capability| reduced_command_names(capability, &config_names))
                .collect();
            let incomplete_prompt_names = snapshot
                .reduced_capabilities()
                .iter()
                .filter(|capability| capability.key.kind == CapabilityKind::Command)
                .flat_map(|capability| reduced_command_names(capability, &config_names))
                .collect();
            return AdvertisedEntries {
                entries,
                routes: BTreeMap::new(),
                protocol_authority,
                incomplete_evidence_names,
                incomplete_prompt_names,
            };
        }

        candidates.extend(vendor_catalog_entries(handle, caps));

        let reduced = reduced_commands(caps);
        let routes = reduced
            .iter()
            .map(|(name, command)| (name.clone(), command.pinned))
            .collect();

        // Old resumed sessions can predate evidence snapshots. Preserve their
        // existing pickers while still collapsing normalized synthetic
        // collisions deterministically. Complete snapshots use the reduced
        // routes below as their only authority.
        if caps.capability_evidence.is_none() {
            return deduplicate_legacy_candidates(candidates).into();
        }

        let mut reduced = reduced.into_iter().collect::<Vec<_>>();
        reduced.sort_by(|(left_name, left), (right_name, right)| {
            left.kind
                .cmp(&right.kind)
                .then_with(|| left_name.cmp(right_name))
        });
        let entries = reduced
            .into_iter()
            .filter_map(|(name, command)| match command.pinned.route {
                DispatchRoute::Hidden => None,
                DispatchRoute::PromptOnly => Some(prompt_entry(handle, &name, &command)),
                DispatchRoute::NativePreferred => candidates
                    .iter()
                    .filter_map(|candidate| {
                        native_candidate_rank(candidate, &name, &command)
                            .map(|rank| (rank, candidate))
                    })
                    .min_by_key(|(rank, candidate)| {
                        (
                            *rank,
                            candidate.description.clone(),
                            candidate.hint.clone().unwrap_or_default(),
                        )
                    })
                    .map(|(_, candidate)| candidate.clone())
                    .or_else(|| native_fallback_entry(handle, &name, &command)),
            })
            .collect();

        AdvertisedEntries {
            entries,
            routes,
            protocol_authority: BTreeSet::new(),
            incomplete_evidence_names: BTreeSet::new(),
            incomplete_prompt_names: BTreeSet::new(),
        }
    }

    /// Build CommandEntry rows from cached config_options. Each entry's
    /// `arg_picker_spec` is set from the synthesizer output.
    pub fn entries(handle: &str, opts: &[SessionConfigOption]) -> AdvertisedEntries {
        synthesize(opts)
            .into_iter()
            .map(|adv: AdvertisedCommand| CommandEntry {
                name: adv.name,
                description: adv.description,
                hint: adv.hint,
                source: CommandSource::Advertised {
                    handle: handle.to_string(),
                },
                dispatch: Dispatch::SetSessionConfigOption {
                    config_id: adv.config_id,
                },
                arg_picker_spec: Some(adv.arg_picker_spec),
            })
            .collect::<Vec<_>>()
            .into()
    }
}

fn deduplicate_legacy_candidates(candidates: Vec<CommandEntry>) -> Vec<CommandEntry> {
    let mut selected =
        BTreeMap::<String, (usize, (u8, String, String, String), CommandEntry)>::new();
    for (index, mut candidate) in candidates.into_iter().enumerate() {
        let name = normalize_command_name(&candidate.name);
        candidate.name.clone_from(&name);
        let rank = legacy_candidate_rank(&candidate);
        match selected.get_mut(&name) {
            Some((_, current_rank, current)) if rank < *current_rank => {
                *current_rank = rank;
                *current = candidate;
            }
            Some(_) => {}
            None => {
                selected.insert(name, (index, rank, candidate));
            }
        }
    }
    let mut selected = selected.into_values().collect::<Vec<_>>();
    selected.sort_by_key(|(first_index, _, _)| *first_index);
    selected
        .into_iter()
        .map(|(_, _, candidate)| candidate)
        .collect()
}

fn legacy_candidate_rank(entry: &CommandEntry) -> (u8, String, String, String) {
    let (rank, dispatch_id) = match &entry.dispatch {
        Dispatch::SetSessionConfigOption { config_id } => (0, config_id.clone()),
        Dispatch::SetSessionModel => (1, "model".to_owned()),
        Dispatch::SetSessionEffort => (1, "effort".to_owned()),
        Dispatch::SetSessionMode => (1, "mode".to_owned()),
        Dispatch::VendorExec { command, .. } => (2, command.clone()),
        Dispatch::PromptText { normalized } => (3, normalized.clone()),
        Dispatch::Local { .. } => (4, String::new()),
    };
    (
        rank,
        dispatch_id,
        entry.description.clone(),
        entry.hint.clone().unwrap_or_default(),
    )
}

pub(crate) fn normalize_command_name(name: &str) -> String {
    name.trim()
        .trim_start_matches('/')
        .trim()
        .to_ascii_lowercase()
}

pub fn pinned_route_for_command(
    caps: &SpurAgentCaps,
    command_name: &str,
) -> Option<PinnedCapabilityRoute> {
    reduced_commands(caps)
        .get(&normalize_command_name(command_name))
        .map(|command| command.pinned)
}

fn reduced_commands(caps: &SpurAgentCaps) -> BTreeMap<String, ReducedCommand> {
    let Some(snapshot) = caps.capability_evidence.as_ref() else {
        return BTreeMap::new();
    };
    if !snapshot.is_complete() {
        return BTreeMap::new();
    }
    let config_names = synthesize_advertised(caps)
        .into_iter()
        .map(|command| (command.config_id, command.name))
        .collect::<BTreeMap<_, _>>();
    let mut commands = BTreeMap::new();

    for capability in snapshot.reduced_capabilities() {
        let names = reduced_command_names(capability, &config_names);
        for name in names {
            let pinned = PinnedCapabilityRoute {
                evidence_epoch: capability.evidence_epoch,
                route: effective_dispatch_route(caps, capability),
            };
            let incoming = ReducedCommand {
                kind: capability.key.kind.clone(),
                upstream_id: capability.key.upstream_id.clone(),
                choices: capability
                    .choices
                    .iter()
                    .cloned()
                    .map(|choice| (choice.id.clone(), choice))
                    .collect(),
                pinned,
            };
            merge_reduced_command(&mut commands, name, incoming);
        }
    }
    commands
}

fn effective_dispatch_route(caps: &SpurAgentCaps, capability: &ReducedCapability) -> DispatchRoute {
    if capability.route != DispatchRoute::PromptOnly
        || capability.key.kind != CapabilityKind::Effort
        || capability.sources.provenances.as_slice() != [EvidenceProvenance::VendorAdvertisement]
        || !caps.supports_grok_set_model()
    {
        return capability.route;
    }

    let Some(display) = caps.grok_display.as_ref() else {
        return capability.route;
    };
    let Some(model_id) = display.model_id.as_deref() else {
        return capability.route;
    };
    let advertised_efforts = display.efforts_for_model(model_id);
    let has_matching_advertisement = !advertised_efforts.is_empty()
        && capability.choices.iter().any(|choice| {
            advertised_efforts
                .iter()
                .any(|effort| effort.id == choice.id)
        });

    if has_matching_advertisement {
        // Grok's real, model-scoped effort catalog is also the contract used
        // by its native `session/set_model` adapter. Permit the first native
        // dispatch from that advertisement; any recorded native failure adds
        // another provenance and therefore keeps subsequent routing prompt-only.
        DispatchRoute::NativePreferred
    } else {
        capability.route
    }
}

fn reduced_command_names(
    capability: &ReducedCapability,
    config_names: &BTreeMap<String, String>,
) -> Vec<String> {
    match &capability.key.kind {
        CapabilityKind::Model => vec!["model".to_owned()],
        CapabilityKind::Effort => vec!["effort".to_owned()],
        CapabilityKind::Mode => vec!["mode".to_owned()],
        CapabilityKind::Command => capability
            .choices
            .iter()
            .map(|choice| normalize_command_name(&choice.id))
            .filter(|name| !name.is_empty())
            .collect(),
        CapabilityKind::Custom(_) => config_names
            .get(&capability.key.upstream_id)
            .map(|name| vec![normalize_command_name(name)])
            .unwrap_or_default(),
    }
}

fn merge_reduced_command(
    commands: &mut BTreeMap<String, ReducedCommand>,
    name: String,
    incoming: ReducedCommand,
) {
    let Some(current) = commands.get_mut(&name) else {
        commands.insert(name, incoming);
        return;
    };

    if incoming.pinned.route > current.pinned.route {
        *current = incoming;
    } else if incoming.pinned.route == current.pinned.route {
        for (id, choice) in incoming.choices {
            current.choices.entry(id).or_insert(choice);
        }
    }
}

fn native_candidate_rank(
    entry: &CommandEntry,
    normalized_name: &str,
    command: &ReducedCommand,
) -> Option<u8> {
    if normalize_command_name(&entry.name) != normalized_name {
        return None;
    }
    match (&command.kind, &entry.dispatch) {
        (_, Dispatch::SetSessionConfigOption { config_id })
            if config_id == &command.upstream_id =>
        {
            Some(0)
        }
        (CapabilityKind::Model, Dispatch::SetSessionModel)
        | (CapabilityKind::Effort, Dispatch::SetSessionEffort)
        | (CapabilityKind::Mode, Dispatch::SetSessionMode) => Some(1),
        _ => None,
    }
}

fn prompt_entry(handle: &str, name: &str, command: &ReducedCommand) -> CommandEntry {
    CommandEntry {
        name: name.to_owned(),
        description: format!("Use the agent's /{name} command"),
        hint: None,
        source: CommandSource::Advertised {
            handle: handle.to_owned(),
        },
        dispatch: Dispatch::PromptText {
            normalized: format!("/{name}"),
        },
        arg_picker_spec: static_choice_picker(command),
    }
}

fn native_fallback_entry(
    handle: &str,
    name: &str,
    command: &ReducedCommand,
) -> Option<CommandEntry> {
    let (description, dispatch) = match command.kind {
        CapabilityKind::Model => ("Switch model for this session", Dispatch::SetSessionModel),
        CapabilityKind::Mode => ("Switch agent session mode", Dispatch::SetSessionMode),
        CapabilityKind::Effort | CapabilityKind::Command | CapabilityKind::Custom(_) => {
            return None;
        }
    };
    Some(CommandEntry {
        name: name.to_owned(),
        description: description.to_owned(),
        hint: None,
        source: CommandSource::Advertised {
            handle: handle.to_owned(),
        },
        dispatch,
        arg_picker_spec: static_choice_picker(command),
    })
}

fn static_choice_picker(command: &ReducedCommand) -> Option<ArgPickerSpec> {
    let choices = command
        .choices
        .values()
        .map(|choice| ArgPickerChoice {
            value: choice.id.clone(),
            label: choice.label.clone(),
            description: choice.description.clone(),
        })
        .collect::<Vec<_>>();
    (!choices.is_empty()).then_some(ArgPickerSpec {
        free_text_hint: String::new(),
        typed_hint: Some(ArgPickerHint::StaticChoices { choices }),
    })
}

fn mode_entries(handle: &str, caps: &SpurAgentCaps) -> Vec<CommandEntry> {
    let Some(modes) = caps.modes.as_ref().filter(|_| caps.supports_set_mode()) else {
        return Vec::new();
    };
    vec![CommandEntry {
        name: "mode".to_string(),
        description: "Switch agent session mode".to_string(),
        hint: Some("[mode]".to_string()),
        source: CommandSource::Advertised {
            handle: handle.to_string(),
        },
        dispatch: Dispatch::SetSessionMode,
        arg_picker_spec: Some(ArgPickerSpec {
            free_text_hint: String::new(),
            typed_hint: Some(ArgPickerHint::StaticChoices {
                choices: modes
                    .available_modes
                    .iter()
                    .map(|mode| ArgPickerChoice {
                        value: mode.id.0.to_string(),
                        label: mode.name.clone(),
                        description: None,
                    })
                    .collect(),
            }),
        }),
    }]
}

/// Direct `/model` and `/effort` entries from the vendor planes of the
/// unified [`ModelEffortCatalog`] (spec 2026-09-23 §3.5). The standard
/// config-option entries are synthesized separately above, so the vendor
/// planes only contribute when the standard plane is empty
/// ([`ModelEffortCatalog::vendor_from_caps`]).
fn vendor_catalog_entries(handle: &str, caps: &SpurAgentCaps) -> Vec<CommandEntry> {
    let Some(catalog) = ModelEffortCatalog::vendor_from_caps(caps) else {
        return Vec::new();
    };
    let picker = |choices: &[CatalogChoice]| {
        (!choices.is_empty()).then(|| ArgPickerSpec {
            free_text_hint: String::new(),
            typed_hint: Some(ArgPickerHint::StaticChoices {
                choices: choices
                    .iter()
                    .map(|choice| ArgPickerChoice {
                        value: choice.value.clone(),
                        label: choice.label.clone(),
                        description: choice.description.clone(),
                    })
                    .collect(),
            }),
        })
    };
    let mut entries = Vec::new();
    if !catalog.models.is_empty() {
        entries.push(CommandEntry {
            name: "model".to_string(),
            description: "Switch model for this session".to_string(),
            hint: catalog
                .current_model
                .as_deref()
                .map(|value| current_choice_label(&catalog.models, value)),
            source: CommandSource::Advertised {
                handle: handle.to_string(),
            },
            dispatch: Dispatch::SetSessionModel,
            arg_picker_spec: picker(&catalog.models),
        });
    }
    if !catalog.efforts.is_empty() {
        entries.push(CommandEntry {
            name: "effort".to_string(),
            description: "Switch reasoning / thinking effort".to_string(),
            hint: catalog
                .current_effort
                .as_deref()
                .map(|value| current_choice_label(&catalog.efforts, value)),
            source: CommandSource::Advertised {
                handle: handle.to_string(),
            },
            dispatch: Dispatch::SetSessionEffort,
            arg_picker_spec: picker(&catalog.efforts),
        });
    }
    entries
}

/// The current selection's display label, falling back to its raw value
/// when the catalog carries no labeled choice for it.
fn current_choice_label(choices: &[CatalogChoice], current: &str) -> String {
    choices
        .iter()
        .find(|choice| choice.value == current)
        .map(|choice| choice.label.clone())
        .unwrap_or_else(|| current.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::LocalLayer;
    use spur_acp::capability_evidence::{
        CapabilityChoice, CapabilityKey, CapabilityKind, CliIdentity, EvidenceClaim, EvidenceEpoch,
        EvidenceEpochId, EvidenceProvenance, EvidenceRecord, EvidenceSessionScope, ObservationTime,
        RawEvidenceDigest,
    };
    use spur_acp::spur_agent_caps::CapabilityEvidenceSnapshot;
    use spur_acp::{
        AgentKind, InitializeResponse, NewSessionResponse, ProtocolVersion, SessionConfigId,
        SessionConfigOption, SessionConfigSelectOption, SessionMode, SessionModeId,
        SessionModeState, SpurAgentCaps,
    };

    fn evidence_identity() -> CliIdentity {
        CliIdentity {
            resolved_executable: std::path::PathBuf::from("/usr/bin/test-acp"),
            upstream_version: Some("1.0.0".to_owned()),
            argv_fingerprint: "argv".to_owned(),
            environment_fingerprint: "env".to_owned(),
        }
    }

    fn model_evidence(
        identity: &CliIdentity,
        claim: EvidenceClaim,
        provenance: EvidenceProvenance,
    ) -> EvidenceRecord {
        capability_evidence(
            identity,
            CapabilityKind::Model,
            "model",
            claim,
            provenance,
            &[("test-model", "Test Model")],
        )
    }

    fn capability_evidence(
        identity: &CliIdentity,
        kind: CapabilityKind,
        upstream_id: &str,
        claim: EvidenceClaim,
        provenance: EvidenceProvenance,
        choices: &[(&str, &str)],
    ) -> EvidenceRecord {
        EvidenceRecord {
            key: CapabilityKey {
                kind,
                upstream_id: upstream_id.to_owned(),
            },
            claim,
            provenance,
            identity: identity.clone(),
            observed_at: ObservationTime(1),
            raw_digest: RawEvidenceDigest("sha256:model".to_owned()),
            session_scope: EvidenceSessionScope::Session("sid".to_owned()),
            choices: choices
                .iter()
                .map(|(id, label)| CapabilityChoice {
                    id: (*id).to_owned(),
                    label: (*label).to_owned(),
                    description: None,
                })
                .collect(),
        }
    }

    fn with_complete_evidence(
        mut caps: SpurAgentCaps,
        epoch_id: u64,
        records: Vec<EvidenceRecord>,
    ) -> SpurAgentCaps {
        let identity = evidence_identity();
        let epoch = EvidenceEpoch::new(EvidenceEpochId(epoch_id), identity.clone(), records)
            .expect("test evidence must use one identity");
        let snapshot = CapabilityEvidenceSnapshot::from_epoch(epoch, &identity);
        let mut wire = serde_json::to_value(snapshot).expect("snapshot must serialize");
        wire["completeness"] = serde_json::json!("complete");
        caps.capability_evidence = Some(
            serde_json::from_value(wire).expect("complete evidence snapshot must deserialize"),
        );
        caps
    }

    fn caps_with_modes() -> SpurAgentCaps {
        let init = InitializeResponse::new(ProtocolVersion::LATEST);
        let mut new = NewSessionResponse::new(spur_acp::AcpSessionId::new("sid"));
        new.modes = Some(SessionModeState::new(
            SessionModeId::new("read-only"),
            vec![
                SessionMode::new(SessionModeId::new("read-only"), "Ask for approval"),
                SessionMode::new(SessionModeId::new("agent"), "Agent"),
                SessionMode::new(
                    SessionModeId::new("agent-full-access"),
                    "Agent (full access)",
                ),
            ],
        ));
        let caps = SpurAgentCaps::new(&init, &new, AgentKind::CodexAcp);
        let identity = evidence_identity();
        with_complete_evidence(
            caps,
            2,
            vec![capability_evidence(
                &identity,
                CapabilityKind::Mode,
                "mode",
                EvidenceClaim::NativeVerified,
                EvidenceProvenance::StandardAdvertisement,
                &[
                    ("read-only", "Ask for approval"),
                    ("agent", "Agent"),
                    ("agent-full-access", "Agent (full access)"),
                ],
            )],
        )
    }

    #[test]
    fn empty_options_yield_empty_entries() {
        assert!(AdvertisedSource::entries("codex", &[]).is_empty());
    }

    #[test]
    fn allowlisted_option_yields_advertised_entry() {
        let opt = SessionConfigOption::select(
            SessionConfigId::new("model".to_string()),
            "label".to_string(),
            "gpt-5-codex".to_string(),
            vec![SessionConfigSelectOption::new(
                "gpt-5-codex".to_string(),
                "GPT-5 Codex".to_string(),
            )],
        );
        let entries = AdvertisedSource::entries("codex", &[opt]);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "model");
        assert!(matches!(
            &entries[0].source,
            CommandSource::Advertised { handle } if handle == "codex"
        ));
        assert!(matches!(
            &entries[0].dispatch,
            Dispatch::SetSessionConfigOption { config_id } if config_id == "model"
        ));
        assert!(entries[0].arg_picker_spec.is_some());
    }

    #[test]
    fn model_caps_yield_model_advertised_entry() {
        let init = InitializeResponse::new(ProtocolVersion::LATEST);
        let mut new = NewSessionResponse::new(spur_acp::AcpSessionId::new("sid"));
        new.config_options = Some(vec![SessionConfigOption::select(
            SessionConfigId::new("model"),
            "Model",
            "gemini-3.1-pro-preview",
            vec![SessionConfigSelectOption::new(
                "gemini-3.1-pro-preview",
                "Gemini 3.1 Pro Preview",
            )],
        )]);
        let identity = evidence_identity();
        let caps = with_complete_evidence(
            SpurAgentCaps::new(&init, &new, AgentKind::Gemini),
            3,
            vec![capability_evidence(
                &identity,
                CapabilityKind::Model,
                "model",
                EvidenceClaim::NativeVerified,
                EvidenceProvenance::StandardAdvertisement,
                &[("gemini-3.1-pro-preview", "Gemini 3.1 Pro Preview")],
            )],
        );
        let entries = AdvertisedSource::entries_from_caps("gemini", &caps);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "model");
    }

    #[test]
    fn agent_modes_yield_mode_entry_with_advertised_ids_and_labels() {
        let entries = AdvertisedSource::entries_from_caps("codex", &caps_with_modes());
        let mode = entries
            .iter()
            .find(|entry| entry.name == "mode")
            .expect("advertised modes must synthesize /mode");
        let spec = mode
            .arg_picker_spec
            .as_ref()
            .expect("/mode must expose the agent mode catalog");
        let Some(ArgPickerHint::StaticChoices { choices }) = spec.typed_hint.as_ref() else {
            panic!("/mode must use static advertised choices");
        };
        assert_eq!(
            choices
                .iter()
                .map(|choice| (choice.value.as_str(), choice.label.as_str()))
                .collect::<Vec<_>>(),
            vec![
                ("read-only", "Ask for approval"),
                ("agent", "Agent"),
                ("agent-full-access", "Agent (full access)"),
            ]
        );
    }

    fn grok_caps() -> SpurAgentCaps {
        let mut init = InitializeResponse::new(ProtocolVersion::LATEST);
        init.meta = Some(
            serde_json::json!({
                "modelState": {
                    "currentModelId": "grok-4.6",
                    "availableModels": [
                        {
                            "modelId": "grok-4.6",
                            "name": "Grok 4.6",
                            "_meta": {
                                "reasoningEffort": "high",
                                "reasoningEfforts": [
                                    {"id": "xhigh", "label": "Extra High Effort"},
                                    {"id": "high", "label": "High Effort"},
                                    {"id": "medium", "label": "Medium Effort"},
                                    {"id": "low", "label": "Low Effort"}
                                ]
                            }
                        },
                        {
                            "modelId": "grok-composer-2.5-fast",
                            "name": "Grok Composer 2.5 Fast",
                            "_meta": {"reasoningEfforts": []}
                        }
                    ]
                }
            })
            .as_object()
            .expect("meta fixture must be an object")
            .clone(),
        );
        let caps = SpurAgentCaps::new(
            &init,
            &NewSessionResponse::new(spur_acp::AcpSessionId::new("sid")),
            AgentKind::Grok,
        );
        let identity = evidence_identity();
        with_complete_evidence(
            caps,
            4,
            vec![
                capability_evidence(
                    &identity,
                    CapabilityKind::Model,
                    "model",
                    EvidenceClaim::NativeVerified,
                    EvidenceProvenance::AcceptedActiveProbe,
                    &[
                        ("grok-4.6", "Grok 4.6"),
                        ("grok-composer-2.5-fast", "Grok Composer 2.5 Fast"),
                    ],
                ),
                capability_evidence(
                    &identity,
                    CapabilityKind::Effort,
                    "reasoning_effort",
                    EvidenceClaim::NativeVerified,
                    EvidenceProvenance::AcceptedActiveProbe,
                    &[
                        ("xhigh", "Extra High Effort"),
                        ("high", "High Effort"),
                        ("medium", "Medium Effort"),
                        ("low", "Low Effort"),
                    ],
                ),
            ],
        )
    }

    #[test]
    fn grok_catalog_yields_dedicated_model_and_effort_entries() {
        let caps = grok_caps();
        assert!(!caps.supports_set_config_option());

        let entries = AdvertisedSource::entries_from_caps("grok", &caps);
        assert_eq!(
            entries
                .iter()
                .map(|entry| entry.name.as_str())
                .collect::<Vec<_>>(),
            vec!["model", "effort"]
        );
        assert!(matches!(entries[0].dispatch, Dispatch::SetSessionModel));
        assert!(matches!(entries[1].dispatch, Dispatch::SetSessionEffort));
        let effort_spec = entries[1]
            .arg_picker_spec
            .as_ref()
            .expect("effort command must have a picker");
        assert!(matches!(
            effort_spec.typed_hint.as_ref(),
            Some(spur_acp::adapter::arg_picker_hint::ArgPickerHint::StaticChoices { choices })
                if choices.iter().map(|choice| choice.value.as_str()).collect::<Vec<_>>()
                    == vec!["xhigh", "high", "medium", "low"]
        ));

        let mut registry = crate::registry::CommandRegistry::new(LocalLayer::empty());
        registry.set_advertised_commands("grok", entries);
        let visible = registry.available_commands_for_session(Some(&caps));
        assert!(visible.iter().any(|entry| entry.name == "model"));
        assert!(visible.iter().any(|entry| entry.name == "effort"));
    }

    #[test]
    fn grok_composer_model_hides_effort_entry_after_notification() {
        let mut caps = grok_caps();
        assert!(caps.apply_grok_model_changed(&serde_json::json!({
            "sessionId": "sid",
            "update": {
                "sessionUpdate": "model_changed",
                "model_id": "grok-composer-2.5-fast"
            }
        })));

        let entries = AdvertisedSource::entries_from_caps("grok", &caps);
        assert_eq!(
            entries
                .iter()
                .map(|entry| entry.name.as_str())
                .collect::<Vec<_>>(),
            vec!["model"]
        );
    }

    fn kiro_caps() -> SpurAgentCaps {
        let init = InitializeResponse::new(ProtocolVersion::LATEST);
        let mut new = NewSessionResponse::new(spur_acp::AcpSessionId::new("sid"));
        new.meta = Some(
            serde_json::json!({
                "spur.recoveredModels": {
                    "availableModels": [
                        {"modelId": "auto", "name": "auto", "description": "task-picked"},
                        {
                            "modelId": "claude-sonnet-4.5",
                            "name": "claude-sonnet-4.5",
                            "description": "Claude Sonnet 4.5 model"
                        }
                    ],
                    "currentModelId": "claude-sonnet-4.5"
                }
            })
            .as_object()
            .expect("meta fixture must be an object")
            .clone(),
        );
        let caps = SpurAgentCaps::new(&init, &new, AgentKind::Kiro);
        let identity = evidence_identity();
        with_complete_evidence(
            caps,
            5,
            vec![capability_evidence(
                &identity,
                CapabilityKind::Model,
                "model",
                EvidenceClaim::NativeVerified,
                EvidenceProvenance::AcceptedActiveProbe,
                &[("auto", "auto"), ("claude-sonnet-4.5", "claude-sonnet-4.5")],
            )],
        )
    }

    #[test]
    fn kiro_recovered_catalog_yields_dedicated_model_entry() {
        let caps = kiro_caps();
        assert!(!caps.supports_set_config_option());
        assert!(caps.supports_kiro_set_model());

        let entries = AdvertisedSource::entries_from_caps("kiro", &caps);
        assert_eq!(
            entries
                .iter()
                .map(|entry| entry.name.as_str())
                .collect::<Vec<_>>(),
            vec!["model"]
        );
        assert!(matches!(entries[0].dispatch, Dispatch::SetSessionModel));
        let model_spec = entries[0]
            .arg_picker_spec
            .as_ref()
            .expect("model command must have a picker");
        assert!(matches!(
            model_spec.typed_hint.as_ref(),
            Some(spur_acp::adapter::arg_picker_hint::ArgPickerHint::StaticChoices { choices })
                if choices.iter().map(|c| c.value.as_str()).collect::<Vec<_>>()
                    == vec!["auto", "claude-sonnet-4.5"]
        ));

        let mut registry = crate::registry::CommandRegistry::new(LocalLayer::empty());
        registry.set_advertised_commands("kiro", entries);
        let visible = registry.available_commands_for_session(Some(&caps));
        assert!(visible.iter().any(|entry| entry.name == "model"));
    }

    #[test]
    fn grok_standard_and_recovered_model_collision_reduces_to_one_native_entry() {
        let identity = evidence_identity();
        let mut caps = grok_caps();
        caps.config_options = vec![SessionConfigOption::select(
            SessionConfigId::new("model"),
            "Model",
            "test-model",
            vec![SessionConfigSelectOption::new("test-model", "Test Model")],
        )];
        let caps = with_complete_evidence(
            caps,
            11,
            vec![model_evidence(
                &identity,
                EvidenceClaim::NativeVerified,
                EvidenceProvenance::StandardAdvertisement,
            )],
        );

        let entries = AdvertisedSource::entries_from_caps("grok", &caps);
        let models = entries
            .iter()
            .filter(|entry| entry.name == "model")
            .collect::<Vec<_>>();

        assert_eq!(models.len(), 1, "one reduced capability has one entry");
        assert!(matches!(
            &models[0].dispatch,
            Dispatch::SetSessionConfigOption { config_id } if config_id == "model"
        ));
        assert!(
            entries
                .iter()
                .all(|entry| !matches!(entry.dispatch, Dispatch::SetSessionModel)),
            "the vendor SetSessionModel entry stays suppressed while the standard plane won"
        );
    }

    #[test]
    fn kiro_prompt_model_collision_keeps_one_prompt_dispatch_path() {
        let identity = evidence_identity();
        let caps = with_complete_evidence(
            kiro_caps(),
            12,
            vec![model_evidence(
                &identity,
                EvidenceClaim::CandidateObserved,
                EvidenceProvenance::VendorAdvertisement,
            )],
        );
        let mut registry = crate::registry::CommandRegistry::new(LocalLayer::empty());
        registry.set_agent_commands(
            "kiro",
            vec![CommandEntry {
                name: " /MODEL ".to_owned(),
                description: "Agent prompt model".to_owned(),
                hint: None,
                source: CommandSource::Agent {
                    handle: "kiro".to_owned(),
                },
                dispatch: Dispatch::PromptText {
                    normalized: "/model".to_owned(),
                },
                arg_picker_spec: None,
            }],
        );
        registry
            .set_advertised_commands("kiro", AdvertisedSource::entries_from_caps("kiro", &caps));

        let models = registry
            .available_commands_for_session(Some(&caps))
            .into_iter()
            .filter(|entry| entry.name == "model")
            .collect::<Vec<_>>();

        assert_eq!(models.len(), 1, "prompt and native paths must not coexist");
        assert!(matches!(models[0].dispatch, Dispatch::PromptText { .. }));
    }

    #[test]
    fn hidden_reduced_model_is_absent_even_when_agent_advertises_prompt_command() {
        let identity = evidence_identity();
        let caps = with_complete_evidence(
            kiro_caps(),
            13,
            vec![model_evidence(
                &identity,
                EvidenceClaim::Inconclusive,
                EvidenceProvenance::InconclusiveFailure,
            )],
        );
        let mut registry = crate::registry::CommandRegistry::new(LocalLayer::empty());
        registry.set_agent_commands(
            "kiro",
            vec![CommandEntry {
                name: "model".to_owned(),
                description: "Agent prompt model".to_owned(),
                hint: None,
                source: CommandSource::Agent {
                    handle: "kiro".to_owned(),
                },
                dispatch: Dispatch::PromptText {
                    normalized: "/model".to_owned(),
                },
                arg_picker_spec: None,
            }],
        );
        registry
            .set_advertised_commands("kiro", AdvertisedSource::entries_from_caps("kiro", &caps));

        assert!(registry
            .available_commands_for_session(Some(&caps))
            .iter()
            .all(|entry| entry.name != "model"));
    }
}
