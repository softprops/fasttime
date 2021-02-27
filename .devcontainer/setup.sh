apt-get update
apt-get install -y \
  curl \
  git \
  gnupg2 \
  jq \
  sudo \
  zsh \
  vim \
  build-essential \
  openssl \
  llvm-dev \
  libclang-dev \
  clang

## Install rustup and common components
curl https://sh.rustup.rs -sSf | sh -s -- -y

source $HOME/.cargo/env

rustup install 1.49.0
rustup component add rustfmt --toolchain nightly
rustup component add clippy

# add wasm target
rustup target add wasm32-wasi --toolchain 1.49.0

# macro expansion debugging
cargo install cargo-expand

# rapid dependency experimenting
cargo install cargo-edit

# hot reloading
cargo install cargo-watch

## setup and install oh-my-zsh
#sh -c "$(curl -fsSL https://raw.githubusercontent.com/robbyrussell/oh-my-zsh/master/tools/install.sh)"
#cp -R /root/.oh-my-zsh /home/$USERNAME
#cp /root/.zshrc /home/$USERNAME
#sed -i -e "s/\/root\/.oh-my-zsh/\/home\/$USERNAME\/.oh-my-zsh/g" /home/$USERNAME/.zshrc
#chown -R $USER_UID:$USER_GID /home/$USERNAME/.oh-my-zsh /home/$USERNAME/.zshrc
