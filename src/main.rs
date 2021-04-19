use handlebars::{to_json, Handlebars};
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
use std::{env, thread, time};
use structopt::StructOpt;

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
async fn main() -> Result<(), kube::Error> {
    std::env::set_var("RUST_LOG", "info,kube=info");
    env_logger::init();

    let opt = Opt::from_args();
    if env::var("OPERATOR").is_err() {
        render_manifests(opt);
        return Ok(());
    }

    let client = Client::try_default().await?;
    let api_svc: Api<Service> = Api::namespaced(client, &opt.namespace);
    let svc = api_svc.get(&opt.svc_watch).await?;
    let svc_selector = svc.spec.unwrap().selector.unwrap();
    let (key, val) = svc_selector.iter().next().unwrap();
    let client = Client::try_default().await?;
    let api_pod: Api<Pod> = Api::namespaced(client, &opt.namespace);
    let pods = api_pod
        .list(&ListParams::default().labels(format!("{}={}", key, val).as_str()))
        .await?;
    let ips = pods
        .iter()
        .map(|pod| pod.status.as_ref().unwrap().pod_ip.as_ref().unwrap())
        .collect();
    poll_metrics(ips, opt.min_success_rate).await?;
    log::info!(
        "Minimum success rate attained for {}, switching over {}",
        &opt.svc_watch,
        &opt.svc_failover
    );
    let mut backends = retrieve_backends(&opt.namespace, &opt.ts_name).await?;
    switch_backends(&mut backends, &opt.svc_watch, &opt.svc_failover);
    patch_ts(&opt.namespace, &opt.ts_name, backends).await?;
    Ok(())
}

fn render_manifests(opt: Opt) {
    let mut data = Map::new();
    data.insert("args".to_string(), to_json(&opt));

    let mut hb = Handlebars::new();
    hb.set_strict_mode(true);
    hb.register_template_string("manifests", include_str!("templates/manifests.yml"))
        .unwrap();
    println!("{}", hb.render("manifests", &to_json(&data)).unwrap());
}

async fn poll_metrics(ips: Vec<&String>, min_success_rate: f64) -> Result<(), kube::Error> {
    let client = HyperClient::new();
    let re_success: Regex = Regex::new(
        r#"(?m)^response_total\{.*target_addr=".*:(\d+)".*classification="success".* (\d+)"#,
    )
    .unwrap();
    let re_failure: Regex = Regex::new(
        r#"(?m)^response_total\{.*target_addr=".*:(\d+)".*classification="failure".* (\d+)"#,
    )
    .unwrap();
    loop {
        let mut total_success = 0.0;
        let mut total_failure = 0.0;
        for ip in &ips {
            let uri = format!("http://{}:4191/metrics", ip).parse().unwrap();
            let resp = client.get(uri).await?;
            let bytes = body::to_bytes(resp.into_body()).await?;
            let body = String::from_utf8(bytes.to_vec())?;
            for cap in re_success.captures_iter(&body) {
                if &cap[1] == "4191" {
                    continue;
                }
                total_success = total_success + cap[2].parse::<f64>().unwrap();
            }
            for cap in re_failure.captures_iter(&body) {
                if &cap[1] == "4191" {
                    continue;
                }
                total_failure = total_failure + cap[2].parse::<f64>().unwrap();
            }
        }
        let success_rate = total_success / (total_success + total_failure);
        log::info!("Success rate: {:.0}%", success_rate * 100.0);
        if success_rate < min_success_rate {
            break;
        }
        thread::sleep(time::Duration::from_secs(5));
    }
    Ok(())
}

async fn retrieve_backends(ns: &str, ts: &str) -> Result<Vec<Backend>, kube::Error> {
    let client = Client::try_default().await?;
    let ts_api: Api<TrafficSplit> = Api::namespaced(client, ns);
    let ts = ts_api.get(ts).await?;
    Ok(ts.spec.backends)
}

fn switch_backends(backends: &mut Vec<Backend>, watched: &str, failover: &str) {
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
