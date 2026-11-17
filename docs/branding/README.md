# Meridian branding

The logo family for Meridian core and its plugins. The idea behind every mark is the
project's own: one DSL for everything.

| Module | Logo | Category |
| --- | --- | --- |
| Core | <img src="logos/meridian-core.svg" width="48" alt="Meridian Core logo"> | Platform |
| Terraform | <img src="logos/meridian-terraform.svg" width="48" alt="Meridian Terraform plugin logo"> | Infrastructure as code |
| Bicep | <img src="logos/meridian-bicep.svg" width="48" alt="Meridian Bicep plugin logo"> | Infrastructure as code |
| CloudFormation | <img src="logos/meridian-cloudformation.svg" width="48" alt="Meridian CloudFormation plugin logo"> | Infrastructure as code |
| DSC | <img src="logos/meridian-dsc.svg" width="48" alt="Meridian DSC plugin logo"> | Configuration management |
| Ansible | <img src="logos/meridian-ansible.svg" width="48" alt="Meridian Ansible plugin logo"> | Configuration management |
| Puppet | <img src="logos/meridian-puppet.svg" width="48" alt="Meridian Puppet plugin logo"> | Configuration management |
| Chef | <img src="logos/meridian-chef.svg" width="48" alt="Meridian Chef plugin logo"> | Configuration management |
| Docker | <img src="logos/meridian-docker.svg" width="48" alt="Meridian Docker plugin logo"> | Containers |
| Kubernetes | <img src="logos/meridian-kubernetes.svg" width="48" alt="Meridian Kubernetes plugin logo"> | Orchestration |

## The system

- **The white point** at the top of the globe is the single DSL. It appears in every mark.
- **The core mark** is a globe drawn from four meridians, one per plugin category, all
  meeting at that point.
- **Plugin marks** keep the same tile, globe and point. Only the glyph changes, drawn in
  its category's colour.
- The glyphs are original. They deliberately do not reuse any vendor's logo, so they can
  ship with the project without trademark concerns.

## Colours

| Use | Colour |
| --- | --- |
| Tile gradient | `#22357A` to `#0A112B` |
| Globe, point | `#FFFFFF` |
| Infrastructure as code | `#6F9BFF` |
| Configuration management | `#FF8A4C` |
| Containers | `#35D0C2` |
| Orchestration | `#B48CFF` |

## Files

Each module has two files in [`logos/`](logos/), drawn on a 64 by 64 grid:

- `meridian-<module>.svg` is the full-colour tile.
- `meridian-<module>-mono.svg` has no tile and uses `currentColor`, for single-colour
  contexts such as dark surfaces, terminals and print.

## Adding a logo for a new plugin

Copy an existing plugin file from the same category, keep the tile, ring, faint meridians
and white point unchanged, and replace only the glyph group. Keep the glyph to simple
3.5 unit round-capped strokes so it stays legible at 16 px, and use the category colour
from the table above.
