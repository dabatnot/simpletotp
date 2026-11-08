# SimpleTOTP

Application Linux de bureau pour les codes TOTP. Cliquez sur un service dans le menu de notification pour copier son code courant.

## Utilisation

- **Ajouter** : saisissez un service, un compte et sa clé Base32, ou collez une adresse `otpauth://totp/`.
- **Importer une image** : ouvrez une image contenant un QR TOTP ou un QR d'export Google Authenticator. Si Google affiche plusieurs QR, importez-les successivement.
- **Scanner avec la caméra** : présentez le QR devant une webcam. L'import par image reste disponible si aucune caméra n'est détectée.
- **QR** : affichez le QR d'un compte pour le scanner sur un téléphone.
- Fermez la fenêtre pour laisser l'application dans la zone de notification ; choisissez **Quitter** dans son menu pour l'arrêter.

Les comptes sont conservés dans le coffre de secrets de la session Linux via Secret Service. Le dépôt et le répertoire de l'application ne contiennent aucune clé. Un QR affiché ou une image d'import révèle toutefois la clé à quiconque peut le voir ou accéder au fichier.

## Installation sur Fedora

GTK 4 et ses fichiers de développement sont nécessaires à la compilation. La session doit fournir Secret Service et une zone de notification compatible StatusNotifierItem (par exemple l'extension GNOME AppIndicator).

```sh
sudo dnf install gtk4-devel
./install.sh
```

Le script installe le programme dans `~/.local/bin`, ajoute une entrée au menu des applications et active le démarrage à la connexion. Pour développer : `cargo test`, puis `cargo run`.

Seuls les comptes TOTP sont pris en charge. Les comptes HOTP contenus dans un export Google sont ignorés.
