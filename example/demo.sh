#!/usr/bin/env bash

# Set up a cluster locally and install linkerd
k3d cluster create --wait
linkerd install | kubectl apply -f -
linkerd check

# Set up and inject the sample app which contains a trafficsplit
linkerd inject example/emojivoto.yml | kubectl apply -f -

# You can start watching how the trafficsplit evolves in a separate console
watch -n 1 -d 'kubectl -n emojivoto get ts voting -ojson | jq .spec'

# Initially, all the traffic is sent to voting-svc, which should fail when
# voting for the doughnut. The second service voting-svc-alt doesn't fail for
# the dougnut, but it's initially disabled (this is a local service but might
# as well be a service mirrored from another cluster).
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
linkerd-failover -n emojivoto --ts voting --svc-watch voting-svc --svc-failover voting-svc-alt --min-success-rate 0.5 | kubectl apply -f -

# And in a separate console tail the failover logs
kubectl -n linkerd-failover logs -f -l component=failover

# Set up a port-forward to the web UI
# And open your browser at http://localhost:8080
kubectl -n emojivoto port-forward svc/web-svc 8080:80 &

# Now start voting for the doughnut, which should fail. When the success rate
# gets below 50% you'll see all the weight being switched to voting-svc-alt and
# voting for the doughnut won't fail anymore! Note this completes the
# linkerd-failover job and it'll need to be set up again in order to start
# watching another configuration.
