#!/usr/bin/env sh
set -eu

# Downloads an Anna release from GitHub and unpacks it into ~/.local/.

main() {
    platform="$(uname -s)"
    arch="$(uname -m)"
    channel="${ZED_CHANNEL:-stable}"
    repo="Workspaacing/anna"
    ZED_VERSION="${ZED_VERSION:-latest}"
    if [ "$ZED_VERSION" = "latest" ]; then
        download_base="https://github.com/$repo/releases/latest/download"
    else
        download_base="https://github.com/$repo/releases/download/v$ZED_VERSION"
    fi
    # Use TMPDIR if available (for environments with non-standard temp directories)
    if [ -n "${TMPDIR:-}" ] && [ -d "${TMPDIR}" ]; then
        temp="$(mktemp -d "$TMPDIR/anna-XXXXXX")"
    else
        temp="$(mktemp -d "/tmp/anna-XXXXXX")"
    fi

    if [ "$platform" = "Darwin" ]; then
        platform="macos"
    elif [ "$platform" = "Linux" ]; then
        platform="linux"
    else
        echo "Unsupported platform $platform"
        exit 1
    fi

    case "$platform-$arch" in
        macos-arm64* | linux-arm64* | linux-aarch64)
            arch="aarch64"
            ;;
        macos-x86* | linux-x86*)
            arch="x86_64"
            ;;
        *)
            echo "Unsupported platform or architecture"
            exit 1
            ;;
    esac

    if command -v curl >/dev/null 2>&1; then
        curl () {
            command curl -fL "$@"
        }
    elif command -v wget >/dev/null 2>&1; then
        curl () {
            wget -O- "$@"
        }
    else
        echo "Could not find 'curl' or 'wget' in your path"
        exit 1
    fi

    "$platform" "$@"

    if [ "$(command -v anna)" = "$HOME/.local/bin/anna" ]; then
        echo "Anna has been installed. Run with 'anna'"
    else
        echo "To run Anna from your terminal, you must add ~/.local/bin to your PATH"
        echo "Run:"

        case "$SHELL" in
            *zsh)
                echo "   echo 'export PATH=\$HOME/.local/bin:\$PATH' >> ~/.zshrc"
                echo "   source ~/.zshrc"
                ;;
            *fish)
                echo "   fish_add_path -U $HOME/.local/bin"
                ;;
            *)
                echo "   echo 'export PATH=\$HOME/.local/bin:\$PATH' >> ~/.bashrc"
                echo "   source ~/.bashrc"
                ;;
        esac

        echo "To run Anna now, '~/.local/bin/anna'"
    fi
}

linux() {
    if [ -n "${ZED_BUNDLE_PATH:-}" ]; then
        cp "$ZED_BUNDLE_PATH" "$temp/anna-linux-$arch.tar.gz"
    else
        echo "Downloading Anna version: $ZED_VERSION"
        curl "$download_base/anna-linux-$arch.tar.gz" > "$temp/anna-linux-$arch.tar.gz"
    fi

    suffix=""
    if [ "$channel" != "stable" ]; then
        suffix="-$channel"
    fi

    appid=""
    case "$channel" in
      stable)
        appid="com.workspaacing.Anna"
        ;;
      dev)
        appid="com.workspaacing.Anna-Dev"
        ;;
      *)
        echo "Unknown release channel: ${channel}. Using stable app ID."
        appid="com.workspaacing.Anna"
        ;;
    esac

    # Unpack
    rm -rf "$HOME/.local/anna$suffix.app"
    mkdir -p "$HOME/.local/anna$suffix.app"
    tar -xzf "$temp/anna-linux-$arch.tar.gz" -C "$HOME/.local/"

    zed_editor="$HOME/.local/anna$suffix.app/libexec/anna-editor"
    if [ -f "$zed_editor" ] && command -v ldd >/dev/null 2>&1; then
        missing="$(ldd "$zed_editor" 2>/dev/null | sed -n 's/^[[:space:]]*\(.*\) => not found$/\1/p')"
        if [ -n "$missing" ]; then
            echo "Warning: your system is missing libraries that Anna needs:"
            echo "$missing" | sed 's/^/    /'
            echo "Install them with your package manager, or Anna will fail to start."
        fi
    fi

    # Setup ~/.local directories
    mkdir -p "$HOME/.local/bin" "$HOME/.local/share/applications"

    # Link the binary
    ln -sf "$HOME/.local/anna$suffix.app/bin/anna" "$HOME/.local/bin/anna"

    # Copy .desktop file
    desktop_file_path="$HOME/.local/share/applications/${appid}.desktop"
    src_dir="$HOME/.local/anna$suffix.app/share/applications"
    cp "$src_dir/${appid}.desktop" "${desktop_file_path}"
    sed -i "s|Icon=anna|Icon=$HOME/.local/anna$suffix.app/share/icons/hicolor/512x512/apps/anna.png|g" "${desktop_file_path}"
    sed -i "s|Exec=anna|Exec=$HOME/.local/anna$suffix.app/bin/anna|g" "${desktop_file_path}"
}

macos() {
    echo "Downloading Anna version: $ZED_VERSION"
    curl "$download_base/Anna-$arch.dmg" > "$temp/Anna-$arch.dmg"
    hdiutil attach -quiet "$temp/Anna-$arch.dmg" -mountpoint "$temp/mount"
    app="$(cd "$temp/mount/"; echo *.app)"
    echo "Installing $app"
    if [ -d "/Applications/$app" ]; then
        echo "Removing existing $app"
        rm -rf "/Applications/$app"
    fi
    ditto "$temp/mount/$app" "/Applications/$app"
    hdiutil detach -quiet "$temp/mount"

    mkdir -p "$HOME/.local/bin"
    # Link the binary
    ln -sf "/Applications/$app/Contents/MacOS/cli" "$HOME/.local/bin/anna"
}

main "$@"
