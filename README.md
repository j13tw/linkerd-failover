# linkerd-failover

Service failover operator for Kubernetes.

## Description

Kubernetes operator that given two services referred in a
[Trafficsplit][trafficsplit], it watches the success rate of one of them, and if
it drops below a given threshold, the weights are updated automatically so all
the traffic gets redirected to the second service.

The success rate metrics are aggregated across all the service pod replicas,
which should be injected by linkerd.

## Disclaimer and Caveats

This is just a proof of concept! Not suitable for production...

Also, the calculated success rate is done over the entire lifetime of the
service. An improved implementation would query Prometheus to calculate that
rate over some recent time window.

## Installation

Just grab the latest [release] and set up a Trafficsplit and the failover
controller as detailed in the example below.

## Usage

```
linkerd-failover 0.9.0
Service failover operator for Kubernetes

USAGE:
    linkerd-failover --min-success-rate <min-success-rate> --namespace <namespace> --svc-failover <svc-failover> --svc-watch <svc-watch> --ts <ts-name>

FLAGS:
    -h, --help       Prints help information
    -V, --version    Prints version information

OPTIONS:
        --min-success-rate <min-success-rate>    Success rate threshold triggering the fail-over
    -n, --namespace <namespace>                  Namespace for the Trafficsplit and services
        --svc-failover <svc-failover>            Service to fail-over to
        --svc-watch <svc-watch>                  Service to watch
        --ts <ts-name>                           Trafficsplit name
```

## Example

(Taken from [/example/demo.sh](/example/demo.sh))

```bash
# Set up a cluster locally and install linkerd
$ k3d cluster create --wait
$ linkerd install | kubectl apply -f -
$ linkerd check

# Set up and inject the sample app which contains a trafficsplit
$ linkerd inject example/emojivoto.yml | kubectl apply -f -

# You can start watching how the trafficsplit evolves in a separate console
$ watch -d 'kubectl -n emojivoto get ts voting -ojson | jq .spec'

# Initially, all the traffic is sent to voting-svc, which should fail when
# voting for the doughnut. The second service voting-svc-alt doesn't fail for
# the dougnut, but it's initially disabled (this is a local service but might
# as well be a service mirrored from another cluster, to implement cross-cluster
failover).
#{
#  "backends": [
#    {
#      "service": "voting-svc",
#      "weight": 1
#    },
#    {
#      "service": "voting-svc-alt",
#      "weight": 0
#    }
#  ],
#  "service": "voting-svc"
#}

# Set up linkerd-failover
$ linkerd-failover -n emojivoto --ts voting --svc-watch voting-svc --svc-failover voting-svc-alt --min-success-rate 0.5 | kubectl apply -f -

# And in a separate console tail the failover logs
$ kubectl -n linkerd-failover logs -f -l component=failover

# Set up a port-forward to the web UI
# And open your browser at http://localhost:8080
$ kubectl -n emojivoto port-forward svc/web-svc 8080:80 &
```

Now start voting for the doughnut, which should fail. When the success rate
gets below 50% you'll see all the weight being switched to voting-svc-alt and
voting for the doughnut won't fail anymore! Note this completes the
linkerd-failover job and it'll need to be set up again in order to start
watching another configuration.

## License

linkerd-await is copyright 2021 the Linkerd authors. All rights reserved.

Licensed under the Apache License, Version 2.0 (the "License"); you may not use
these files except in compliance with the License. You may obtain a copy of the
License at

    http://www.apache.org/licenses/LICENSE-2.0

Unless required by applicable law or agreed to in writing, software distributed
under the License is distributed on an "AS IS" BASIS, WITHOUT WARRANTIES OR
CONDITIONS OF ANY KIND, either express or implied. See the License for the
specific language governing permissions and limitations under the License.

<!-- refs -->
[trafficsplit]: https://linkerd.io/2.10/features/traffic-split/
[release]: https://github.com/alpeb/linkerd-failover/releases
