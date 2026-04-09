use std::sync::Arc;

use anyhow::{Result, bail};
use salvo::prelude::*;

use crate::{
    config::AtomicConfig,
    gateway::{HttpClient, RequestStats},
};

pub fn setup_handler_state(
    depot: &Depot,
) -> Result<(&Arc<AtomicConfig>, &Arc<RequestStats>, &Arc<HttpClient>)> {
    // 获取配置、统计和 HTTP 客户端
    let Ok(config) = depot.obtain::<Arc<AtomicConfig>>() else {
        bail!("AtomicConfig not found in depot");
    };
    let Ok(stats) = depot.obtain::<Arc<RequestStats>>() else {
        bail!("RequestStats not found in depot");
    };
    let Ok(client) = depot.obtain::<Arc<HttpClient>>() else {
        bail!("HttpClient not found in depot");
    };
    Ok((config, stats, client))
}
