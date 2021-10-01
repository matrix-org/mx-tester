# About

`mx-tester` is a WIP tool to help test Matrix bots and Synapse modules.

# What `mx-tester` does

The flow of `mx-tester` is the following:

1. `mx-tester build` Create a Docker image for Synapse. If you need your Synapse to be setup with custom modules, you add scripts to this step to install the modules.
2. `mx-tester up` Launch Synapse on port 9999.
3. `mx-tester run` Launch your test script.
4. `mx-tester down` Stop Synapse.

You may customize each of these steps by inserting scripts.

If you are using `mx-tester` to launch tests on a project that has already been setup, your flow is probably simply:


```sh
# Setup and launch Synapse, modules, bots, ...
$ mx-tester build
$ mx-tester up
# Launch the tests. Repeat as many times as needed, as you fix tests or your code.
$ mx-tester run
# Once you're done testing
$ mx-tester down
```

# Setting up `mx-tester`.

`mx-tester` requires a configuration file, typically called `mx-tester.yml`.

It has the following structure:

```yaml
name: A name for this test suite
homeserver_config:
  # A yaml subtree specifying configuration options for Synapse.
  #
  # This uses the exact same syntax as homeserver.yaml.
  #
  # If you make use of modules, it MUST contain an entry `modules`,
  # as per https://github.com/matrix-org/synapse/blob/develop/docs/modules.md
modules:
  - name: Name of a module you wish to setup
    build: # A script to setup the module.
      - # This script MUST copy the source code of the module
      - # to directory $MX_TEST_MODULE_DIR
      - # ...
      - # env: MX_TEST_MODULE_DIR -- where the module should be copied
      - # env: MX_TEST_SYNAPSE_DIR -- where Synapse source lies
      - # env: MX_TEST_SCRIPT_TMPDIR -- a temporary directory where the test can
      - #   write data. Note that `mx-tester` will NOT clear this directory.
      - # env: MX_TEST_CWD -- the directory in which the test was launched.
  - # Other modules, if necessary.
up: # Optionally, a script to be executed at the end of `mx-tester up`
  - # Use this script e.g. to setup additional components, such as bots.
  -
  - # env: MX_TEST_SCRIPT_TMPDIR -- a temporary directory where the test can
  - #   write data. Note that `mx-tester` will NOT clear this directory.
  - # env: MX_TEST_CWD -- the directory in which the test was launched.
run: # Optionally, a script to be executed as `mx-tester run`
  - # Use this script e.g. to start your tests.
  - # env: MX_TEST_SCRIPT_TMPDIR -- a temporary directory where the test can
  - #   write data. Note that `mx-tester` will NOT clear this directory.
  - # env: MX_TEST_CWD -- the directory in which the test was launched.
down: # Optionally, a script to be executed at the start of `mx-tester down`
      # Use this script e.g. to teardown additional components, such as bots.
    success: # Optionally, a script to be executed if `run` was a success. -- NOT IMPLEMENTED YET
    failure: # Optionally, a script to be executed if `run` was a failure. -- NOT IMPLEMENTED YET
    finally: # Optionally, a script to be executed regardless of the result of `run`.
      - # env: MX_TEST_SCRIPT_TMPDIR -- a temporary directory where the test can
      - #   write data. Note that `mx-tester` will NOT clear this directory.
      - # env: MX_TEST_CWD -- the directory in which the test was launched.
```
