# Empacotamento Flatpak do Clypse

Arquivos para publicar o Clypse no Flathub, seguindo o
[guia de submissão do Flathub](https://docs.flathub.org/docs/for-app-authors/submission).

## Arquivos

- `io.github.felipe_abreu.Clypse.json` — manifest do Flatpak.
- `cargo-sources.json` — fontes offline geradas a partir do `Cargo.lock`
  (o build no Flathub roda sem rede). **Regenere a cada mudança no lock:**

  ```bash
  make flatpak-sources
  ```

- `flatpak-cargo-generator.py` — script oficial de
  [flatpak/flatpak-builder-tools](https://github.com/flatpak/flatpak-builder-tools/tree/master/cargo)
  (vendorizado para conveniência; requer `aiohttp` e `toml`).

## Antes de submeter

1. Crie e envie a tag da release (`git tag v0.2.0 && git push --tags`).
2. Preencha o campo `commit` do manifest com o hash completo da tag:
   `git rev-list -n1 v0.2.0`.
3. O repositório do GitHub precisa estar **público** — o build bot do
   Flathub clona as fontes e a verificação do App ID exige acesso.

## Build local

```bash
flatpak install flathub org.gnome.Platform//49 org.gnome.Sdk//49 \
    org.freedesktop.Sdk.Extension.rust-stable//25.08
flatpak install flathub org.flatpak.Builder

flatpak run org.flatpak.Builder --force-clean --user --install \
    builddir flatpak/io.github.felipe_abreu.Clypse.json

flatpak run io.github.felipe_abreu.Clypse
```

> O branch da extensão rust-stable deve casar com a base freedesktop do
> SDK GNOME escolhido (verifique com `flatpak remote-info flathub org.gnome.Sdk//49`).

## Lint (o mesmo que o Flathub roda na revisão)

```bash
flatpak run --command=flatpak-builder-lint org.flatpak.Builder \
    manifest flatpak/io.github.felipe_abreu.Clypse.json
```
