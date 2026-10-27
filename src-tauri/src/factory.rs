//! Maps stored accounts to connectors. Each new service crate is plugged in here.

use agora_core::demo::DemoService;
use agora_core::model::{Account, ServiceKind};
use agora_core::secrets;
use agora_core::service::{Service, ServiceFactory};
use agora_mattermost::MattermostService;
use agora_rocketchat::{Credentials, RocketChatService};
use std::sync::Arc;

pub struct Factory;

impl ServiceFactory for Factory {
    fn create(&self, account: &Account) -> Result<Arc<dyn Service>, String> {
        let secret = || secrets::get(&secrets::account_key(&account.id))?.ok_or_else(|| "Not signed in".to_string());
        match account.kind {
            ServiceKind::Demo => Ok(Arc::new(DemoService::new(&account.id))),
            ServiceKind::Mattermost => Ok(Arc::new(MattermostService::new(account, secret()?))),
            ServiceKind::RocketChat => {
                let creds = Credentials::parse(&secret()?).ok_or("Not signed in")?;
                Ok(Arc::new(RocketChatService::new(account, creds)))
            }
            ServiceKind::Slack => {
                let creds = agora_slack::Credentials::parse(&secret()?).ok_or("Not signed in")?;
                Ok(Arc::new(agora_slack::SlackService::new(account, creds)))
            }
        }
    }
}
