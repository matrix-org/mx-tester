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

# --- Configuring external components

up:
  # Optional. A script to be executed to prepare additional components
  # as part of `mx-tester up`.
  before:
    - # Optional. Script to be executed before bringinng up the image.
    - # Use this script e.g. to setup databases, etc.
  after:
    - # Optional. Script to be executed after bringing up the image.
    - # Use this script e.g. to setup bots.

down:
  # Optional. A script to be executed to clean up the environment,
  # typically to teardown components that were setup with `up`. This
  # script will be executed at the end of `mx-tester down`.
  success:
    - # Optional. A script to be executed only if `run` was a success.
    - # This script will ONLY be executed if `mx-tester` is called to
    - # execute both run and down, e.g. `mx-tester up run down`.
  failure:
    - # Optional. A script to be executed only if `run` was a failure.
    - # This script will ONLY be executed if `mx-tester` is called to
    - # execute both run and down, e.g. `mx-tester up run down`.
  finally:
    - # Optional. A script to be executed regardless of the result of `run`.
    - # This script will ALWAYS be executed if `mx-tester down` is called.

directories:
  # Optional. Directories to use for the test.
  root:
    # Optional. The root directory for this test.
    # All temporary files and logs are created as subdirectories of this directory.
    # Default: `mx-tester` in the platform's temporary directory.
    # May be overridden from the command-line with parameter `--root`.
    #
    # IMPORTANT: Some CI environments (e.g. Docker-in-docker aka dind) do not play
    # well with the platform's temporary directory. If you are running `mx-tester`
    # in such an environment, you should probably specify your root directory to
    # be in your (environment-specific) home-style directory (which may be your
    # build directory, etc.).
    #
    # Example: On GitLab CI, you should probably use `/builds/$CI_PROJECT_PATH`.

# --- Configuring the test

run:
  - # Optional. A script to be executed as `mx-tester run`.
  - # Use this e.g. to launch integration tests.

users:
  - # Optional. A list of users to create for the test.
  - # These users are created during `mx-tester up`.
  - # If the users already exist, they are not recreated.
  - localname:
    # Required. A name for the user.
    admin:
    # Optional. If `true`, the users should be an admin for the server.
    # Default: `false`.
    password:
    # Optional. If specified, a password for the user.
    # Default: "password".
    rate_limit:
    # Optional. If `unlimited`, remove rate limits for this user.
    # Default: Use the global setting for rate limits.
    rooms:
    - # Optional. A list of rooms to create.
    - public:
      # Optional. If `true`, the room should be public.
      # Default: `false`.
      name:
      # Optional. A name for the room.
      # Default: No name.
      alias:
      # Optional. An alias for the room.
      # Default: No alias.
      # If there is already a room with the same alias, the old alias will
      # be unregistered (we assume that this was caused by a previous call
      # to `mx-tester up`).
      # It several rooms define the same `alias` in the same mx-tester.yml,
      # this is an error.
      topic:
      # Optional. A topic for the room.
      # Default: No topic.
      members:
      # Optional. A list of users (created by `users`) to invite to the room.
      # mx-tester will ensure that these users join the room.
      # Default: No invites.


# --- Configuring the homeserver

synapse:
  # Optionally, a version of Synapse.
  # If unspecified, pick the latest version available on Docker Hub.
  docker:
    # Required: A docker tag, e.g. "matrixdotorg/synapse:latest"

modules:
  # Optionally, a list of modules to install.
  - name: Name of a module you wish to setup
    build:
      # Required: A script to setup the module.
      # This may be as simple as copying the module from its directory
      # to $MX_TEST_MODULE_DIR.
      - # This script MUST copy the source code of the module
      - # to directory $MX_TEST_MODULE_DIR
      - # ...
      - # env: MX_TEST_MODULE_DIR -- where the module should be copied
      - # env: MX_TEST_SYNAPSE_DIR -- where Synapse source lies
      - # env: MX_TEST_SCRIPT_TMPDIR -- a temporary directory where the test can
      - #   write data. Note that `mx-tester` will NOT clear this directory.
      - # env: MX_TEST_CWD -- the directory in which the test was launched.
    install:
      # Optional. A script to install dependencies.
      # Typically, this will be something along the lines of
      # `pypi -r module_name/requirements.txt`
    config:
      # Required. Additional configuration information
      # to copy into homeserver.yaml.
      # This typically looks like
      # module: python_module_name
      # config:
      #   key: value
      #   key: value
      #   ...
  - # Other modules, if necessary.

homeserver:
  # Optional. Additional configuration for the homeserver.
  # Each of these fields will be copied into homeserver.yaml.
  # For more detail on the fields, see the documentation for homeserver.yaml.
  server_name:
    # Optional. The name of a homeserver.
    # By default, `localhost:9999`.
  public_baseurl:
    # Optional. The URL to communicate to the server with.
    # By default, `http://localhost:9999`.
  registration_shared_secret:
    # Optional. The registration shared secret.
    # By default, "MX_TESTER_REGISTRATION_DEFAULT".
  ...
    # Any other field to be copied in homeserver.yaml.

# --- Docker configuration

docker:
  # Optional. Additional configuration for Docker.
  hostname:
    # Optional. The hostname to give to the Synapse container on the docker
    # network.
    # By default, "synapse".
  port_mapping:
    - # Optional. The docker port mapping configuration to use for the
    - # synapse container.
    - # By default:
    - # - host: 9999
    - # - guest: 8008

credentials:
  # Optional. Credentials to connect to a Docker registry,
  # as per `docker login`.
  # Typically useful to adapt mx-tester to your CI.
  username:
  # Optional. Specify a username to connect to the registry.
  # Default: No username.
  # May be overridden from the command-line with parameter `--username`/`-u`.
  password:
  # Optional. Specify a password to connect to the registry.
  # Default: No password.
  # May be overridden from the command-line with parameter `--password`/`-p`.
  serveraddress:
  # Optional. Specify a server to connect to the registry.
  # Default: No server.
  # May be overridden from the command-line with parameter `--server`.

# Optional
workers:
  enabled:
  # A boolean. Specify `true` to launch Synapse with workers.
  # Default: No workers.
  # May be overridden from the command-line with parameter `--workers`.
```

# Docker notes

Everything is executed with Docker, with the same limitations and abstraction leaks.

Host `host.docker.internal` should be configured on all platforms, which gives the guest Synapse (and modules) access to the host, should this be needed for tests.

The guest container is running on a network called `mx-tester-synapse-$(TAG)`,
where `TAG` is the Docker tag for the version of Synapse running. By default,
that's `matrixdotorg/synapse:latest`.
