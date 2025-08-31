FROM fedora:latest

# Base image configured for Nix

RUN dnf -y install sudo git && \
    curl --proto '=https' --tlsv1.2 -sSf -L https://install.determinate.systems/nix | sh -s -- install linux \
    --extra-conf "sandbox = false" \
    --init none \
    --no-confirm
RUN groupadd -g 1000 dev && \
    useradd -u 1000 -g dev -G wheel dev && \
    echo "%wheel ALL=(ALL:ALL) NOPASSWD: ALL" >> /etc/sudoers

# Dev tools

RUN dnf -y copr enable atim/starship && \
    dnf -y copr enable alternateved/eza && \
    dnf -y install fish starship direnv eza ripgrep helix && \
    chsh -s /usr/bin/fish dev
COPY <<"EOF" /etc/fish/config.fish
set fish_greeting
fish_add_path /nix/var/nix/profiles/default/bin
starship init fish | source
direnv hook fish | source

alias ls "eza -l"
alias la "eza -la"
export EDITOR="hx"

cd /workspaces/*
direnv allow
EOF
USER dev
