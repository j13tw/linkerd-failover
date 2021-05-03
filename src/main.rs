#[macro_use]
extern crate lazy_static;
extern crate regex;

use env_logger::Builder;
use futures::future::join_all;
use handlebars::{to_json, Handlebars, RenderError};
use hyper::{body, Client as HyperClient};
use k8s_openapi::api::core::v1::{Pod, Service};
use kube::{
    api::{ListParams, Patch, PatchParams},
    Api, Client, CustomResource,
};
use regex::Regex;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::value::Map;
use std::{env, time};
use structopt::StructOpt;
use tokio::task::JoinHandle;

type Error = Box<dyn std::error::Error + Send + Sync>;

#[derive(Clone, Debug, Serialize, StructOpt)]
#[structopt(about = "Service failover operator for Kubernetes")]
struct Opt {
    #[structopt(short, long, help = "Namespace for the Trafficsplit and services")]
    namespace: String,

    #[structopt(long = "ts", help = "Trafficsplit name")]
    ts_name: String,

    #[structopt(long, help = "Service to watch")]
    svc_watch: String,

    #[structopt(long, help = "Service to fail-over to")]
    svc_failover: String,

    #[structopt(long, help = "Success rate threshold triggering the fail-over [0-1]")]
    min_success_rate: f64,
}

#[derive(CustomResource, Serialize, Deserialize, JsonSchema, Default, Debug, Clone)]
#[kube(
    group = "split.smi-spec.io",
    version = "v1alpha1",
    kind = "TrafficSplit",
    namespaced
)]
pub struct TrafficSplitSpec {
    backends: Vec<Backend>,
}

#[derive(Serialize, Deserialize, Debug, JsonSchema, Clone)]
pub struct Backend {
    service: String,
    weight: u32,
}

#[tokio::main]
async fn main() -> Result<(), Error> {
    let mut builder = Builder::new();
    builder.parse_default_env();
    builder.init();

    let opt = Opt::from_args();
    if env::var("OPERATOR").is_err() {
        render_manifests(opt)?;
        return Ok(());
    }

    let client = Client::try_default().await?;
    let api_svc: Api<Service> = Api::namespaced(client, &opt.namespace);
    let svc = api_svc.get(&opt.svc_watch).await?;
    let svc_selector = svc
        .spec
        .expect("Couldn't retrieve spec from service")
        .selector
        .expect("Couldn't retrieve selector from service spec");
    let (key, val) = svc_selector
        .iter()
        .next()
        .expect("Service selector is empty");
    let client = Client::try_default().await?;
    let api_pod: Api<Pod> = Api::namespaced(client, &opt.namespace);
    let pods = api_pod
        .list(&ListParams::default().labels(format!("{}={}", key, val).as_str()))
        .await?;
    let ips = pods
        .iter()
        .inspect(|pod| {
            if pod.status.is_none() {
                log::warn!(
                    "Pod '{}'doesn't have a status",
                    pod.metadata.name.as_ref().unwrap_or(&"unknown".to_string())
                );
            }
        })
        .filter_map(|pod| pod.status.as_ref())
        .inspect(|status| {
            if status.pod_ip.is_none() {
                log::warn!("Pod doesn't have an IP");
            }
        })
        .filter_map(|status| status.pod_ip.clone())
        .collect();
    poll_metrics(&ips, opt.min_success_rate).await?;
    log::info!(
        "Minimum success rate attained for {}, switching over {}",
        &opt.svc_watch,
        &opt.svc_failover
    );
    let mut backends = retrieve_backends(&opt.namespace, &opt.ts_name).await?;
    swap_backends(&mut backends, &opt.svc_watch, &opt.svc_failover);
    patch_ts(&opt.namespace, &opt.ts_name, backends).await?;
    Ok(())
}

fn render_manifests(opt: Opt) -> Result<(), RenderError> {
    let mut data = Map::new();
    data.insert("args".to_string(), to_json(&opt));

    let mut hb = Handlebars::new();
    hb.set_strict_mode(true);
    hb.register_template_string("manifests", include_str!("templates/manifests.yml"))
        .expect("manifests.yml not found");
    hb.render("manifests", &to_json(&data))
        .map(|x| println!("{}", x))
}

async fn poll_metrics(ips: &Vec<String>, min_success_rate: f64) -> Result<(), Error> {
    loop {
        let mut tasks: Vec<JoinHandle<Result<(f64, f64), Error>>> = Vec::new();
        for ip in ips {
            let ip = ip.clone();
            let task = tokio::task::spawn(async move {
                let mut success = 0.0;
                let mut failure = 0.0;

                let uri = format!("http://{}:4191/metrics", ip).parse().map_err(|e| {
                    log::warn!("Error parsing url: {}", e);
                    e
                })?;
                let client = HyperClient::new();
                let resp = client.get(uri).await.map_err(|e| {
                    log::warn!("Error fetching uri: {}", e);
                    e
                })?;
                let bytes = body::to_bytes(resp.into_body()).await.map_err(|e| {
                    log::warn!("Error retrieving response body: {}", e);
                    e
                })?;
                let body = String::from_utf8(bytes.to_vec()).map_err(|e| {
                    log::warn!("Error parsing response body: {}", e);
                    e
                })?;

                lazy_static! {
                    static ref RE_SUCCESS: Regex = Regex::new(
                        r#"(?m)^response_total\{.*target_addr=".*:(\d+)".*classification="success".* (\d+)"#,
                    )
                    .expect("Failed parsing RE_SUCCESS");
                    static ref RE_FAILURE: Regex = Regex::new(
                        r#"(?m)^response_total\{.*target_addr=".*:(\d+)".*classification="failure".* (\d+)"#,
                    )
                    .expect("Failed parsing RE_FAILURE");
                }

                for cap in RE_SUCCESS.captures_iter(&body) {
                    if &cap[1] == "4191" {
                        continue;
                    }
                    match cap[2].parse::<f64>() {
                        Ok(x) => success = success + x,
                        Err(x) => log::warn!("Invalid metric value: {}: {}", cap[2].to_string(), x),
                    }
                }
                for cap in RE_FAILURE.captures_iter(&body) {
                    if &cap[1] == "4191" {
                        continue;
                    }
                    match cap[2].parse::<f64>() {
                        Ok(x) => failure = failure + x,
                        Err(x) => log::warn!("Invalid metric value: {}: {}", cap[2].to_string(), x),
                    }
                }

                Ok((success, failure))
            });
            tasks.push(task);
        }
        let results = join_all(tasks).await;
        let metrics = results
            .iter()
            .filter_map(|x| x.as_ref().ok().and_then(|x| x.as_ref().ok()));
        let (total_success, total_failure) =
            metrics.fold((0.0, 0.0), |acc, x| (acc.0 + x.0, acc.1 + x.1));
        let success_rate = total_success / (total_success + total_failure);
        log::info!("Success rate: {:.0}%", success_rate * 100.0);
        if success_rate < min_success_rate {
            break;
        }
        tokio::time::sleep(time::Duration::from_secs(5)).await;
    }
    Ok(())
}

async fn retrieve_backends(ns: &str, ts: &str) -> Result<Vec<Backend>, kube::Error> {
    let client = Client::try_default().await?;
    let ts_api: Api<TrafficSplit> = Api::namespaced(client, ns);
    let ts = ts_api.get(ts).await?;
    Ok(ts.spec.backends)
}

fn swap_backends(backends: &mut Vec<Backend>, watched: &str, failover: &str) {
    for backend in backends {
        if backend.service == watched {
            backend.weight = 0
        } else if backend.service == failover {
            backend.weight = 1
        }
    }
}

async fn patch_ts(ns: &str, ts: &str, backends: Vec<Backend>) -> Result<TrafficSplit, kube::Error> {
    let client = Client::try_default().await?;
    let ts_api: Api<TrafficSplit> = Api::namespaced(client, ns);
    let ssapply = PatchParams::apply("linkerd_failover_patch");
    let patch = serde_json::json!({
        "apiVersion": "split.smi-spec.io/v1alpha1",
        "kind": "TrafficSplit",
        "name": "voting",
        "spec": {
            "backends": backends
        }
    });
    ts_api.patch(ts, &ssapply, &Patch::Merge(patch)).await
}
