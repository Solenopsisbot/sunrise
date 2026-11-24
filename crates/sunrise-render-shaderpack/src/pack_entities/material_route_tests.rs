use super::*;

#[test]
fn optional_variants_split_contiguous_materials_without_guessing_from_alpha() {
    let mut vertices = Vec::new();
    for layer in [0, OIT_ENTITY, 0, TRANSLUCENT_PARTICLE_OVERLAY, 0] {
        vertices.extend(
            [Vertex {
                layer: layer | 3 << 16,
                color: 0x40808080,
                ..bytemuck::Zeroable::zeroed()
            }; 4],
        );
    }
    let spans = [
        EntitySpan {
            stage: render_stage("BLOCK_ENTITIES"),
            id: 71,
            ..Default::default()
        },
        EntitySpan {
            start: 8,
            stage: render_stage("PARTICLES"),
            ..Default::default()
        },
    ];
    let collect = |drawn| {
        material_runs(&vertices, &spans, drawn)
            .map(|(_, range, p, _)| (range, p))
            .collect::<Vec<_>>()
    };
    let mut legacy = [true; PROGRAMS];
    legacy[BLOCK_TRANSLUCENT] = false;
    legacy[PARTICLE_TRANSLUCENT] = false;
    assert_eq!(
        collect(legacy),
        [(0..8, Some(BLOCK)), (8..20, Some(PARTICLE))]
    );
    assert_eq!(
        collect([true; PROGRAMS]),
        [
            (0..4, Some(BLOCK)),
            (4..8, Some(BLOCK_TRANSLUCENT)),
            (8..12, Some(PARTICLE)),
            (12..16, Some(PARTICLE_TRANSLUCENT)),
            (16..20, Some(PARTICLE)),
        ]
    );
    let casters = Casters {
        blocks: true,
        ..Default::default()
    };
    assert!(casters.cast(&spans[0], BLOCK_TRANSLUCENT));
    assert_eq!(shadow_program(BLOCK_TRANSLUCENT), BLOCK);
    assert!(!casters.cast(&spans[1], PARTICLE_TRANSLUCENT));
}

/// Sign text in a block entity's run goes to the block program (spec 2.3: `gbuffers_block` draws
/// sign text), split where its depth mode changes: only POLYGON_OFFSET text takes the biased
/// pipeline.
#[test]
fn sign_text_runs_with_the_block_program_split_by_depth_mode() {
    use sunrise_render_core::entity::TEXT_NORMAL_SOLID_OVERLAY;
    let mut vertices = Vec::new();
    for layer in [
        0,
        TEXT_NORMAL_SOLID_OVERLAY,
        TEXT_POLYGON_OFFSET_SOLID_OVERLAY,
        TEXT_POLYGON_OFFSET_SOLID_OVERLAY,
        0,
        TEXT_POLYGON_OFFSET_SOLID_OVERLAY | OIT_ENTITY,
    ] {
        vertices.extend(
            [Vertex {
                layer: layer | 3 << 16,
                ..bytemuck::Zeroable::zeroed()
            }; 4],
        );
    }
    let spans = [EntitySpan {
        stage: render_stage("BLOCK_ENTITIES"),
        id: 71,
        ..Default::default()
    }];
    let runs = material_runs(&vertices, &spans, [true; PROGRAMS])
        .map(|(_, range, p, offset)| (range, p, offset))
        .collect::<Vec<_>>();
    assert_eq!(
        runs,
        [
            (0..8, Some(BLOCK), false),
            (8..16, Some(BLOCK), true),
            (16..20, Some(BLOCK), false),
            (20..24, Some(BLOCK_TRANSLUCENT), true),
        ]
    );
    // Text outside a block entity's run is not the block program's.
    let entity = [EntitySpan {
        stage: render_stage("ENTITIES"),
        id: 72,
        ..Default::default()
    }];
    assert_eq!(
        material_runs(&vertices[8..12], &entity, [true; PROGRAMS])
            .map(|(_, range, p, offset)| (range, p, offset))
            .collect::<Vec<_>>(),
        [(0..4, Some(ENTITY), false)]
    );
}
