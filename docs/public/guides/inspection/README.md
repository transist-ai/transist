# Inspection Guides

Understanding what your workflows are doing is critical. These guides cover the various tools Transist provides for inspecting runs, both from the command line and through graphical interfaces.

- [**Inspecting Runs, Artifacts, and Events**](inspecting-runs-artifacts-evidence-and-events.md)
    This is the official, command-line-first way to inspect your runs. Start here to learn how to use `transist runs` to see the status, event history, and files for any workflow.

- [**The Companion HTTP Server**](companion-http-server-in-practice.md)
    Transist includes a read-only HTTP server that provides another way to inspect runs, including the original workflow source code and the compiled recipe.

- [**Using the Companion Inspection UI**](using-the-companion-inspection-ui.md)
    The companion server also hosts a simple, read-only web UI. It's a great way to browse through runs and visualize the workflow graph in your browser.

- [**Observing Runs with Prometheus**](observing-runs-with-prometheus.md)
    For monitoring and alerting, the companion server exposes a `/metrics` endpoint compatible with Prometheus.

- [**Operating the Transist Home Directory**](operating-the-transist-home-directory.md)
    A look at the underlying directory structure where all run data, artifacts, and evidence are stored.
