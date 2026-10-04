use super::*;

fn ruby_block(below: bool) -> TextBlock {
    let run = |text: &str, scale| TextRun {
        text: text.into(),
        style: TextStyle {
            size_scale: scale,
            ..Default::default()
        },
        link: None,
    };
    TextBlock {
        kind: TextBlockKind::Paragraph,
        content: vec![
            Inline::Text(run("前", 1.0)),
            Inline::Ruby(Box::new(rebook_publication::RubyRun {
                base: vec![run("山路", 1.0)],
                annotation: vec![run("やまみちのながいよみ", 0.5)],
                below,
            })),
            Inline::Text(run("後。次の文章です。", 1.0)),
        ],
        style: rebook_publication::BlockStyle {
            line_height: 1.0,
            ..Default::default()
        },
        source: None,
    }
}

#[test]
fn ruby_keeps_prose_offsets_and_reserves_annotation_space() {
    let mut engine = LayoutEngine::new();
    for below in [false, true] {
        for width in [120.0, 260.0] {
            let prepared = engine.shape_text(&ruby_block(below), &ReaderStyle::default(), width);
            assert_eq!(&*prepared.text, "前山路後。次の文章です。");
            assert_eq!(prepared.ruby.len(), 1);
            let ruby = &prepared.ruby[0];
            assert_eq!(&prepared.text[ruby.range.clone()], "山路");
            assert!(ruby.spacing > 0.0);
            let line = prepared
                .layout
                .lines()
                .find(|line| line.text_range().contains(&ruby.range.start))
                .unwrap();
            assert!(
                line.text_range().start <= ruby.range.start
                    && line.text_range().end >= ruby.range.end
            );
            assert!(
                line.metrics().baseline + ruby.offset_y >= line.metrics().block_min_coord - 0.01
            );
            assert!(
                line.metrics().baseline + ruby.offset_y + ruby.layout.height()
                    <= line.metrics().block_max_coord + 0.01
            );
        }
    }
}
