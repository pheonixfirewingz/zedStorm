use super::{Client, Status, proto};
use anyhow::{Context as _, Result};
use chrono::{DateTime, Utc};
use cloud_api_client::websocket_protocol::MessageToClient;
use cloud_api_client::{
    GetAuthenticatedUserResponse, KnownOrUnknown, Organization, OrganizationId, Plan, PlanInfo,
    UpdateSystemSettingsBody,
};
use cloud_api_types::OrganizationConfiguration;
use collections::HashMap;
use feature_flags::FeatureFlagAppExt;
use futures::{StreamExt, channel::mpsc};
use gpui::{
    App, AppContext as _, Context, EventEmitter, SharedString, SharedUri, Task, TaskExt, WeakEntity,
};
use postage::{sink::Sink, watch};
use rpc::proto::{RequestMessage, UsersResponse};
use std::sync::{Arc, Weak};
use text::ReplicaId;
use util::ResultExt;

pub type LegacyUserId = u64;

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Clone, Copy)]
pub struct ProjectId(pub u64);

impl ProjectId {
    pub fn to_proto(self) -> u64 {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParticipantIndex(pub u32);

#[derive(Default, Debug)]
pub struct User {
    pub legacy_id: LegacyUserId,
    pub username: SharedString,
    pub avatar_uri: SharedUri,
    pub name: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Collaborator {
    pub peer_id: proto::PeerId,
    pub replica_id: ReplicaId,
    pub user_id: LegacyUserId,
    pub is_host: bool,
    pub committer_name: Option<String>,
    pub committer_email: Option<String>,
}

impl PartialOrd for User {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for User {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.username.cmp(&other.username)
    }
}

impl PartialEq for User {
    fn eq(&self, other: &Self) -> bool {
        self.legacy_id == other.legacy_id && self.username == other.username
    }
}

impl Eq for User {}

pub struct UserStore {
    users: HashMap<u64, Arc<User>>,
    participant_indices: HashMap<u64, ParticipantIndex>,
    plan_info: Option<PlanInfo>,
    current_user: watch::Receiver<Option<Arc<User>>>,
    current_organization: Option<Arc<Organization>>,
    organizations: Vec<Arc<Organization>>,
    plans_by_organization: HashMap<OrganizationId, Plan>,
    configuration_by_organization: HashMap<OrganizationId, OrganizationConfiguration>,
    client: Weak<Client>,
    _maintain_current_user: Task<Result<()>>,
    _handle_sign_out: Task<()>,
    weak_self: WeakEntity<Self>,
}

pub enum Event {
    ParticipantIndicesChanged,
    PrivateUserInfoUpdated,
    PlanUpdated,
    OrganizationChanged,
}

impl EventEmitter<Event> for UserStore {}

impl UserStore {
    pub fn new(client: Arc<Client>, cx: &Context<Self>) -> Self {
        let (mut current_user_tx, current_user_rx) = watch::channel();
        let (sign_out_tx, mut sign_out_rx) = mpsc::unbounded();
        client.sign_out_tx.lock().replace(sign_out_tx);
        client.add_message_to_client_handler({
            let this = cx.weak_entity();
            move |message, cx| Self::handle_message_to_client(this.clone(), message, cx)
        });

        Self {
            users: Default::default(),
            current_user: current_user_rx,
            current_organization: None,
            organizations: Vec::new(),
            plans_by_organization: HashMap::default(),
            configuration_by_organization: HashMap::default(),
            plan_info: None,
            participant_indices: Default::default(),
            client: Arc::downgrade(&client),
            _maintain_current_user: cx.spawn(async move |this, cx| {
                let mut status = client.status();
                let weak = Arc::downgrade(&client);
                drop(client);
                while let Some(status) = status.next().await {
                    // If the client is dropped, the app is shutting down.
                    let Some(client) = weak.upgrade() else {
                        return Ok(());
                    };
                    match status {
                        Status::Authenticated
                        | Status::Reauthenticated
                        | Status::Connected { .. } => {
                            if let Some(user_id) = client.user_id() {
                                let system_id =
                                    client.telemetry().system_id().map(|id| id.to_string());
                                let response = client
                                    .cloud_client()
                                    .get_authenticated_user(system_id)
                                    .await
                                    .log_err();

                                let current_user_and_response = if let Some(response) = response {
                                    let user = Arc::new(User {
                                        legacy_id: user_id,
                                        username: response.user.username.clone().into(),
                                        avatar_uri: response.user.avatar_url.clone().into(),
                                        name: response.user.name.clone(),
                                    });

                                    Some((user, response))
                                } else {
                                    None
                                };
                                current_user_tx
                                    .send(
                                        current_user_and_response
                                            .as_ref()
                                            .map(|(user, _)| user.clone()),
                                    )
                                    .await
                                    .ok();

                                cx.update(|cx| {
                                    if let Some((user, response)) = current_user_and_response {
                                        this.update(cx, |this, cx| {
                                            this.users.insert(user_id, user);
                                            this.update_authenticated_user(response, cx)
                                        })
                                    } else {
                                        anyhow::Ok(())
                                    }
                                })?;

                                this.update(cx, |_, cx| cx.notify())?;
                            }
                        }
                        Status::SignedOut => {
                            current_user_tx.send(None).await.ok();
                            this.update(cx, |this, cx| {
                                this.clear_organizations();
                                this.clear_plan_and_usage();
                                cx.emit(Event::PrivateUserInfoUpdated);
                                cx.notify();
                            })?;
                        }
                        Status::ConnectionLost => {
                            this.update(cx, |_, cx| {
                                cx.notify();
                            })?;
                        }
                        _ => {}
                    }
                }
                Ok(())
            }),
            _handle_sign_out: cx.spawn(async move |this, cx| {
                while let Some(()) = sign_out_rx.next().await {
                    let Some(client) = this
                        .read_with(cx, |this, _cx| this.client.upgrade())
                        .ok()
                        .flatten()
                    else {
                        break;
                    };

                    client.sign_out(cx).await;
                }
            }),
            weak_self: cx.weak_entity(),
        }
    }

    #[cfg(feature = "test-support")]
    pub fn clear_cache(&mut self) {
        self.users.clear();
    }

    pub fn get_users(
        &self,
        user_ids: Vec<u64>,
        cx: &Context<Self>,
    ) -> Task<Result<Vec<Arc<User>>>> {
        let mut user_ids_to_fetch = user_ids.clone();
        user_ids_to_fetch.retain(|id| !self.users.contains_key(id));

        cx.spawn(async move |this, cx| {
            if !user_ids_to_fetch.is_empty() {
                this.update(cx, |this, cx| {
                    this.load_users(
                        proto::GetUsers {
                            user_ids: user_ids_to_fetch,
                        },
                        cx,
                    )
                })?
                .await?;
            }

            this.read_with(cx, |this, _| {
                user_ids
                    .iter()
                    .map(|user_id| {
                        this.users
                            .get(user_id)
                            .cloned()
                            .with_context(|| format!("user {user_id} not found"))
                    })
                    .collect()
            })?
        })
    }

    pub fn fuzzy_search_users(
        &self,
        query: String,
        cx: &Context<Self>,
    ) -> Task<Result<Vec<Arc<User>>>> {
        self.load_users(proto::FuzzySearchUsers { query }, cx)
    }

    pub fn get_cached_user(&self, user_id: u64) -> Option<Arc<User>> {
        self.users.get(&user_id).cloned()
    }

    pub fn get_user_optimistic(&self, user_id: u64, cx: &Context<Self>) -> Option<Arc<User>> {
        if let Some(user) = self.users.get(&user_id).cloned() {
            return Some(user);
        }

        self.get_user(user_id, cx).detach_and_log_err(cx);
        None
    }

    pub fn get_user(&self, user_id: u64, cx: &Context<Self>) -> Task<Result<Arc<User>>> {
        if let Some(user) = self.users.get(&user_id).cloned() {
            return Task::ready(Ok(user));
        }

        let load_users = self.get_users(vec![user_id], cx);
        cx.spawn(async move |this, cx| {
            load_users.await?;
            this.read_with(cx, |this, _| {
                this.users
                    .get(&user_id)
                    .cloned()
                    .context("server responded with no users")
            })?
        })
    }

    pub fn current_user(&self) -> Option<Arc<User>> {
        self.current_user.borrow().clone()
    }

    pub fn current_organization(&self) -> Option<Arc<Organization>> {
        self.current_organization.clone()
    }

    pub fn set_current_organization(
        &mut self,
        organization: Arc<Organization>,
        cx: &mut Context<Self>,
    ) -> Task<Result<()>> {
        let is_same_organization = self
            .current_organization
            .as_ref()
            .is_some_and(|current| current.id == organization.id);

        if is_same_organization {
            return Task::ready(Ok(()));
        }

        let organization_id = organization.id.clone();
        self.current_organization.replace(organization);
        cx.emit(Event::OrganizationChanged);
        cx.notify();

        let Some(client) = self.client.upgrade() else {
            return Task::ready(Ok(()));
        };
        let Some(system_id) = client.telemetry().system_id().map(|id| id.to_string()) else {
            // Without a system ID we have no addressable target row on the
            // server, so the selection stays purely session-local.
            return Task::ready(Ok(()));
        };
        let cloud_client = client.cloud_client();

        cx.background_spawn(async move {
            let body = UpdateSystemSettingsBody {
                selected_organization_id: Some(organization_id),
            };
            cloud_client
                .update_system_settings(system_id, body)
                .await
                .context("failed to persist selected organization")?;
            Ok(())
        })
    }

    pub fn organizations(&self) -> &Vec<Arc<Organization>> {
        &self.organizations
    }

    pub fn plan_for_organization(&self, organization_id: &OrganizationId) -> Option<Plan> {
        self.plans_by_organization.get(organization_id).copied()
    }

    pub fn current_organization_configuration(&self) -> Option<&OrganizationConfiguration> {
        let current_organization = self.current_organization.as_ref()?;

        self.configuration_by_organization
            .get(&current_organization.id)
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn set_current_organization_configuration_for_test(
        &mut self,
        organization: Arc<Organization>,
        configuration: OrganizationConfiguration,
        cx: &mut Context<Self>,
    ) {
        self.current_organization = Some(organization.clone());
        self.organizations = vec![organization.clone()];
        self.configuration_by_organization
            .insert(organization.id.clone(), configuration);
        cx.emit(Event::OrganizationChanged);
        cx.notify();
    }

    pub fn plan(&self) -> Option<Plan> {
        #[cfg(debug_assertions)]
        if let Ok(plan) = std::env::var("ZED_SIMULATE_PLAN").as_ref() {
            use cloud_api_client::Plan;

            return match plan.as_str() {
                "free" => Some(Plan::ZedFree),
                "trial" => Some(Plan::ZedProTrial),
                "pro" => Some(Plan::ZedPro),
                _ => {
                    panic!("ZED_SIMULATE_PLAN must be one of 'free', 'trial', or 'pro'");
                }
            };
        }

        if let Some(organization) = &self.current_organization {
            return self.plan_for_organization(&organization.id);
        }

        self.plan_info.as_ref().map(|info| info.plan())
    }

    pub fn subscription_period(&self) -> Option<(DateTime<Utc>, DateTime<Utc>)> {
        self.plan_info
            .as_ref()
            .and_then(|plan| plan.subscription_period)
            .map(|subscription_period| {
                (
                    subscription_period.started_at.0,
                    subscription_period.ended_at.0,
                )
            })
    }

    pub fn trial_started_at(&self) -> Option<DateTime<Utc>> {
        self.plan_info
            .as_ref()
            .and_then(|plan| plan.trial_started_at)
            .map(|trial_started_at| trial_started_at.0)
    }

    /// Returns whether the user's account is too new to use the service.
    ///
    /// This only applies when operating under the user's personal organization,
    /// not a business organization.
    pub fn account_too_young(&self) -> bool {
        if let Some(org) = &self.current_organization {
            if !org.is_personal {
                return false;
            }
        }

        self.plan_info
            .as_ref()
            .map(|plan| plan.is_account_too_young)
            .unwrap_or_default()
    }

    /// Returns whether the current user has overdue invoices and usage should be blocked.
    pub fn has_overdue_invoices(&self) -> bool {
        self.plan_info
            .as_ref()
            .map(|plan| plan.has_overdue_invoices)
            .unwrap_or_default()
    }

    pub fn clear_organizations(&mut self) {
        self.organizations.clear();
        self.current_organization = None;
    }

    pub fn clear_plan_and_usage(&mut self) {
        self.plan_info = None;
    }

    fn update_authenticated_user(
        &mut self,
        response: GetAuthenticatedUserResponse,
        cx: &mut Context<Self>,
    ) {
        let staff = response.user.is_staff && !*feature_flags::ZED_DISABLE_STAFF;
        cx.update_flags(staff, response.feature_flags);
        if let Some(client) = self.client.upgrade() {
            client
                .telemetry
                .set_authenticated_user_info(Some(response.user.metrics_id.clone()), staff);
        }

        self.organizations = response.organizations.into_iter().map(Arc::new).collect();

        self.current_organization = response
            .default_organization_id
            .and_then(|default_organization_id| {
                self.organizations
                    .iter()
                    .find(|organization| organization.id == default_organization_id)
                    .cloned()
            })
            .or_else(|| self.organizations.first().cloned());
        self.plans_by_organization = response
            .plans_by_organization
            .into_iter()
            .map(|(organization_id, plan)| {
                let plan = match plan {
                    KnownOrUnknown::Known(plan) => plan,
                    KnownOrUnknown::Unknown(_) => {
                        // If we get a plan that we don't recognize, fall back to the Free plan.
                        Plan::ZedFree
                    }
                };

                (organization_id, plan)
            })
            .collect();
        self.configuration_by_organization =
            response.configuration_by_organization.into_iter().collect();

        self.plan_info = Some(response.plan);
        cx.emit(Event::PrivateUserInfoUpdated);
    }

    fn handle_message_to_client(this: WeakEntity<Self>, message: &MessageToClient, cx: &App) {
        match message {
            MessageToClient::UserUpdated => {}
            MessageToClient::NotificationsUpdated | MessageToClient::SettingsUpdated => return,
        }

        cx.spawn(async move |cx| {
            let (cloud_client, system_id) = cx
                .update(|cx| {
                    this.read_with(cx, |this, _cx| {
                        this.client.upgrade().map(|client| {
                            let system_id = client.telemetry().system_id().map(|id| id.to_string());
                            (client.cloud_client(), system_id)
                        })
                    })
                })?
                .ok_or(anyhow::anyhow!("Failed to get Cloud client"))?;

            let response = cloud_client.get_authenticated_user(system_id).await?;
            cx.update(|cx| {
                this.update(cx, |this, cx| {
                    this.update_authenticated_user(response, cx);
                })
            })?;

            anyhow::Ok(())
        })
        .detach_and_log_err(cx);
    }

    pub fn watch_current_user(&self) -> watch::Receiver<Option<Arc<User>>> {
        self.current_user.clone()
    }

    fn load_users(
        &self,
        request: impl RequestMessage<Response = UsersResponse>,
        cx: &Context<Self>,
    ) -> Task<Result<Vec<Arc<User>>>> {
        let client = self.client.clone();
        cx.spawn(async move |this, cx| {
            if let Some(rpc) = client.upgrade() {
                let response = rpc.request(request).await.context("error loading users")?;
                let users = response.users;

                this.update(cx, |this, _| this.insert(users))
            } else {
                Ok(Vec::new())
            }
        })
    }

    pub fn insert(&mut self, users: Vec<proto::User>) -> Vec<Arc<User>> {
        let mut ret = Vec::with_capacity(users.len());
        for user in users {
            let user = User::new(user);
            self.users.insert(user.legacy_id, user.clone());
            ret.push(user)
        }
        ret
    }

    pub fn set_participant_indices(
        &mut self,
        participant_indices: HashMap<u64, ParticipantIndex>,
        cx: &mut Context<Self>,
    ) {
        if participant_indices != self.participant_indices {
            self.participant_indices = participant_indices;
            cx.emit(Event::ParticipantIndicesChanged);
        }
    }

    pub fn participant_indices(&self) -> &HashMap<u64, ParticipantIndex> {
        &self.participant_indices
    }

    pub fn participant_names(
        &self,
        user_ids: impl Iterator<Item = u64>,
        cx: &App,
    ) -> HashMap<u64, SharedString> {
        let mut ret = HashMap::default();
        let mut missing_user_ids = Vec::new();
        for id in user_ids {
            if let Some(username) = self.get_cached_user(id).map(|u| u.username.clone()) {
                ret.insert(id, username);
            } else {
                missing_user_ids.push(id)
            }
        }
        if !missing_user_ids.is_empty() {
            let this = self.weak_self.clone();
            cx.spawn(async move |cx| {
                this.update(cx, |this, cx| this.get_users(missing_user_ids, cx))?
                    .await
            })
            .detach_and_log_err(cx);
        }
        ret
    }
}

impl User {
    fn new(message: proto::User) -> Arc<Self> {
        Arc::new(User {
            legacy_id: message.id,
            username: message.username.into(),
            avatar_uri: message.avatar_url.into(),
            name: message.name,
        })
    }
}

impl Collaborator {
    pub fn from_proto(message: proto::Collaborator) -> Result<Self> {
        Ok(Self {
            peer_id: message.peer_id.context("invalid peer id")?,
            replica_id: ReplicaId::new(message.replica_id as u16),
            user_id: message.user_id as LegacyUserId,
            is_host: message.is_host,
            committer_name: message.committer_name,
            committer_email: message.committer_email,
        })
    }
}
