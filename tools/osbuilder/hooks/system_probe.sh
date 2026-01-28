#!/bin/sh

touch /usr/share/keyrings/datadog-archive-keyring.gpg
chmod a+r /usr/share/keyrings/datadog-archive-keyring.gpg
curl https://keys.datadoghq.com/DATADOG_APT_KEY_CURRENT.public | gpg --no-default-keyring --keyring /usr/share/keyrings/datadog-archive-keyring.gpg --import --batch
curl https://keys.datadoghq.com/DATADOG_APT_KEY_CURRENT.public | apt-key add -
echo "deb [signed-by=/usr/share/keyrings/datadog-archive-keyring.gpg] https://apt.datadoghq.com/ stable 7" > /etc/apt/sources.list.d/datadog.list

apt update && apt install -y datadog-agent

cat > /etc/datadog-agent/system-probe.env <<EOF
DD_AUTH_TOKEN_FILE_PATH=\$(ls /run/kata-containers/shared/containers/*-auth/token)
DD_RUNTIME_SECURITY_CONFIG_EVENT_GRPC_SERVER=security-agent
DD_RUNTIME_SECURITY_CONFIG_CMD_SOCKET=/tmp/wp-cmd.sock
DD_RUNTIME_SECURITY_CONFIG_POLICIES_DIR=/tmp
DD_VSOCK_ADDR=host
DD_SYSTEM_PROBE_CONFIG_LOG_FILE=/tmp/system-probe.log
DD_RUNTIME_SECURITY_CONFIG_ENABLED=true
DD_SYSTEM_PROBE_CONFIG_SYSPROBE_SOCKET=/tmp/sysprobe.sock
DD_SYSPROBE_SOCKET=/tmp/sysprobe.sock
DD_EVENT_MONITORING_CONFIG_SOCKET=/tmp/evt.sock
DD_RUNTIME_SECURITY_CONFIG_SOCKET=vsock:5020
DD_RUNTIME_SECURITY_CONFIG_ACTIVITY_DUMP_ENABLED=false
DD_RUNTIME_SECURITY_CONFIG_SECURITY_PROFILE_ENABLED=false
DD_RUNTIME_SECURITY_CONFIG_ACTIVITY_DUMP_LOCAL_STORAGE_OUTPUT_DIRECTORY=/tmp
DD_REMOTE_AGENT_REGISTRY_ENABLED=false
EOF

cat > /usr/local/bin/start-system-probe <<EOF
#!/bin/sh

BASE_PATH="run/kata-containers/shared/containers"

while true; do
    DD_AUTH_TOKEN_FILE_PATH=\$(ls "\$BASE_PATH"/\*-auth/token 2>/dev/null | head -n 1)

    if [ -n "\$DD_AUTH_TOKEN_FILE_PATH" ]; then
        break
    fi

    sleep 1
done

/opt/datadog-agent/embedded/bin/system-probe \$@
EOF

chmod +x /usr/local/bin/start-system-probe

cat > /etc/systemd/system/system-probe.service <<EOF
[Unit]
Description=Datadog System Probe
Requires=sys-kernel-debug.mount
After=network.target sys-kernel-debug.mount

[Service]
Type=simple
PIDFile=/tmp/system-probe.pid
Restart=on-failure
ExecStart=/usr/local/bin/start-system-probe run --pid=/tmp/system-probe.pid
ExecReload=/bin/kill -HUP \$MAINPID
# Since systemd 229, should be in [Unit] but in order to support systemd <229,
# it is also supported to have it here.
StartLimitInterval=10
StartLimitBurst=5

EnvironmentFile=/etc/datadog-agent/system-probe.env

[Install]
WantedBy=multi-user.target
EOF

ln -s '/etc/systemd/system/system-probe.service' '/etc/systemd/system/multi-user.target.wants/system-probe.service'

