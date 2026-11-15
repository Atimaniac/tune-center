//! Edytor krzywej zegar rdzenia / napięcie, w stylu MSI Afterburner.

use super::backend::CurvePoint;
use eframe::egui::{self, vec2, Align2, Color32, FontId, Pos2, Rect, Sense, Shape, Stroke};

/// Maksymalny offset pojedynczego punktu (sterownik i tak ma własne limity).
pub const MAX_DELTA_KHZ: i32 = 1_000_000;

const ACCENT: Color32 = Color32::from_rgb(118, 185, 0);

#[derive(Clone, Copy)]
pub struct EditPoint {
    pub index: usize,
    pub volt_uv: u32,
    /// Częstotliwość fabryczna (bez offsetu punktu).
    pub base_khz: i64,
    /// Offset aktualnie ustawiony w karcie.
    pub applied_khz: i32,
    /// Offset po edycji (jeszcze niezastosowany).
    pub delta_khz: i32,
}

impl EditPoint {
    pub fn volt_mv(&self) -> f32 {
        self.volt_uv as f32 / 1000.0
    }
    pub fn base_mhz(&self) -> f32 {
        self.base_khz as f32 / 1000.0
    }
    pub fn freq_mhz(&self) -> f32 {
        (self.base_khz + self.delta_khz as i64) as f32 / 1000.0
    }
}

#[derive(Default)]
pub struct CurveEditor {
    pub points: Vec<EditPoint>,
    pub selected: Option<usize>,
    dragging: Option<usize>,
    /// Indeks (w tabeli sterownika) punktu, od którego krzywa została
    /// wyrównana w prawo. Dopóki jest ustawiony, po każdym zapisie program
    /// pilnuje, żeby żaden punkt o wyższym napięciu nie miał wyższego zegara.
    pub flat_from: Option<usize>,
}

fn clamp_delta(d: i32) -> i32 {
    d.clamp(-MAX_DELTA_KHZ, MAX_DELTA_KHZ)
}

impl CurveEditor {
    pub fn load(&mut self, pts: &[CurvePoint]) {
        self.points = pts
            .iter()
            .map(|p| EditPoint {
                index: p.index,
                volt_uv: p.volt_uv,
                base_khz: p.freq_khz as i64 - p.delta_khz as i64,
                applied_khz: p.delta_khz,
                delta_khz: p.delta_khz,
            })
            .collect();
        self.dragging = None;
        if self.selected.is_some_and(|i| i >= self.points.len()) {
            self.selected = None;
        }
        if self.flat_position().is_none() {
            self.flat_from = None;
        }
    }

    /// Pozycja na liście punktu, od którego krzywa jest wyrównana.
    fn flat_position(&self) -> Option<usize> {
        let index = self.flat_from?;
        self.points.iter().position(|p| p.index == index)
    }

    /// Napięcie, którego karta nie powinna przekraczać przy wyrównanej krzywej.
    pub fn flat_voltage_mv(&self) -> Option<f32> {
        self.flat_position().map(|i| self.points[i].volt_mv())
    }

    /// Wywoływane po zapisie i ponownym odczycie krzywej z karty. Sterownik
    /// zaokrągla zegar każdego punktu do własnego kroku, więc po "Wyrównaj
    /// w prawo" część punktów o wyższym napięciu może wylądować odrobinę
    /// wyżej niż punkt zaznaczony - a wtedy karta sięga po ich napięcie.
    /// Funkcja obniża takie punkty i zwraca ich liczbę. `round` to numer
    /// kolejnej próby: każda następna dodaje zapas pół kroku (7,5 MHz).
    pub fn enforce_flat(&mut self, round: i32) -> usize {
        let Some(pos) = self.flat_position() else {
            return 0;
        };
        let anchor = self.points[pos].base_khz + self.points[pos].delta_khz as i64;
        let mut lowered = 0;
        for p in self.points.iter_mut().skip(pos + 1) {
            let actual = p.base_khz + p.delta_khz as i64;
            if actual > anchor {
                let excess = (actual - anchor) as i32;
                p.delta_khz = clamp_delta(p.delta_khz - excess - round * 7_500);
                lowered += 1;
            }
        }
        lowered
    }

    pub fn dirty(&self) -> bool {
        self.points.iter().any(|p| p.delta_khz != p.applied_khz)
    }

    pub fn deltas(&self) -> Vec<(usize, i32)> {
        self.points.iter().map(|p| (p.index, p.delta_khz)).collect()
    }

    pub fn set_delta_by_index(&mut self, index: usize, khz: i32) {
        if let Some(p) = self.points.iter_mut().find(|p| p.index == index) {
            p.delta_khz = clamp_delta(khz);
        }
        self.flat_from = None;
    }

    pub fn shift_all(&mut self, khz: i32) {
        for p in &mut self.points {
            p.delta_khz = clamp_delta(p.delta_khz + khz);
        }
    }

    pub fn zero_all(&mut self) {
        for p in &mut self.points {
            p.delta_khz = 0;
        }
        self.flat_from = None;
    }

    pub fn revert(&mut self) {
        for p in &mut self.points {
            p.delta_khz = p.applied_khz;
        }
        self.flat_from = None;
    }

    /// Cofa niezapisaną edycję, ale zostawia informację o wyrównaniu.
    pub fn revert_keep_flat(&mut self) {
        for p in &mut self.points {
            p.delta_khz = p.applied_khz;
        }
    }

    /// Typowy undervolting: wszystkie punkty o wyższym napięciu dostają tę
    /// samą częstotliwość co punkt `i`, więc karta nie sięga po wyższe napięcie.
    pub fn flatten_above(&mut self, i: usize) {
        let Some(sel) = self.points.get(i).copied() else {
            return;
        };
        let target = sel.base_khz + sel.delta_khz as i64;
        for p in self.points.iter_mut().skip(i + 1) {
            p.delta_khz = clamp_delta((target - p.base_khz) as i32);
        }
        self.flat_from = Some(sel.index);
    }

    /// Nadaje wszystkim punktom ten sam offset co punkt `i`.
    pub fn same_offset_as(&mut self, i: usize) {
        let Some(sel) = self.points.get(i).copied() else {
            return;
        };
        for p in &mut self.points {
            p.delta_khz = sel.delta_khz;
        }
        self.flat_from = None;
    }

    /// `live`: bieżący punkt pracy karty (mV, MHz), jeśli znany.
    pub fn ui(&mut self, ui: &mut egui::Ui, live: Option<(f32, f32)>) {
        // Miejsce na kontrolki pod wykresem; w wąskim oknie zawijają się
        // do kilku wierszy, więc zostawiamy im więcej wysokości.
        let controls = if ui.available_width() < 760.0 {
            150.0
        } else {
            90.0
        };
        let height = (ui.available_height() - controls).max(160.0);
        let (response, painter) =
            ui.allocate_painter(vec2(ui.available_width(), height), Sense::click_and_drag());
        let outer = response.rect;
        painter.rect_filled(outer, 6.0, ui.visuals().extreme_bg_color);

        if self.points.is_empty() {
            painter.text(
                outer.center(),
                Align2::CENTER_CENTER,
                crate::i18n::tr("Brak danych krzywej", "No curve data"),
                FontId::proportional(14.0),
                ui.visuals().weak_text_color(),
            );
            return;
        }

        let plot = Rect::from_min_max(outer.min + vec2(58.0, 12.0), outer.max - vec2(14.0, 30.0));

        // Zakresy liczone z krzywej bazowej, żeby skala nie skakała przy przeciąganiu.
        let v_lo = self.points.first().map(|p| p.volt_mv()).unwrap_or(0.0);
        let v_hi = self.points.last().map(|p| p.volt_mv()).unwrap_or(1.0);
        let pad = ((v_hi - v_lo) * 0.03).max(5.0);
        let (v_min, v_max) = (v_lo - pad, v_hi + pad);
        let base_min = self
            .points
            .iter()
            .map(|p| p.base_mhz())
            .fold(f32::MAX, f32::min);
        let base_max = self
            .points
            .iter()
            .map(|p| p.base_mhz())
            .fold(f32::MIN, f32::max);
        let f_min = (((base_min - 300.0) / 250.0).floor() * 250.0).max(0.0);
        let f_max = ((base_max + 500.0) / 250.0).ceil() * 250.0;

        let to_x = |mv: f32| plot.left() + (mv - v_min) / (v_max - v_min) * plot.width();
        let to_y = |mhz: f32| plot.bottom() - (mhz - f_min) / (f_max - f_min) * plot.height();
        let from_y = |y: f32| f_min + (plot.bottom() - y) / plot.height() * (f_max - f_min);

        // --- interakcja ---
        let xs: Vec<f32> = self.points.iter().map(|p| to_x(p.volt_mv())).collect();
        let nearest = |pos: Pos2| -> Option<usize> {
            xs.iter()
                .enumerate()
                .map(|(i, x)| (i, (x - pos.x).abs()))
                .filter(|(_, d)| *d < 14.0)
                .min_by(|a, b| a.1.total_cmp(&b.1))
                .map(|(i, _)| i)
        };
        let pointer = response.interact_pointer_pos();
        let hit = pointer.and_then(nearest);
        let hover = response.hover_pos().and_then(nearest);
        let shift = ui.input(|i| i.modifiers.shift);

        if response.drag_started() {
            self.dragging = hit;
            if hit.is_some() {
                self.selected = hit;
            }
        }
        if response.clicked() && hit.is_some() {
            self.selected = hit;
        }
        if response.dragged() {
            if let (Some(i), Some(pos)) = (self.dragging, pointer) {
                let y = pos.y.clamp(plot.top(), plot.bottom());
                let p = self.points[i];
                let raw_mhz = from_y(y) - p.base_mhz();
                let new = clamp_delta(raw_mhz.round() as i32 * 1000);
                if shift {
                    self.shift_all(new - p.delta_khz);
                } else {
                    self.points[i].delta_khz = new;
                    // Ręczna zmiana punktu kończy pilnowanie wyrównania.
                    self.flat_from = None;
                }
            }
        }
        if response.drag_stopped() {
            self.dragging = None;
        }

        // --- siatka i osie ---
        let grid = Stroke::new(1.0_f32, ui.visuals().widgets.noninteractive.bg_stroke.color);
        let text = ui.visuals().weak_text_color();
        let font = FontId::proportional(11.0);

        let v_step = if v_max - v_min > 600.0 { 100.0 } else { 50.0 };
        let mut v = (v_min / v_step).ceil() * v_step;
        while v <= v_max {
            let x = to_x(v);
            painter.line_segment(
                [Pos2::new(x, plot.top()), Pos2::new(x, plot.bottom())],
                grid,
            );
            painter.text(
                Pos2::new(x, plot.bottom() + 4.0),
                Align2::CENTER_TOP,
                format!("{v:.0}"),
                font.clone(),
                text,
            );
            v += v_step;
        }
        let f_step = if f_max - f_min > 2500.0 { 500.0 } else { 250.0 };
        let mut f = f_min;
        while f <= f_max {
            let y = to_y(f);
            painter.line_segment(
                [Pos2::new(plot.left(), y), Pos2::new(plot.right(), y)],
                grid,
            );
            painter.text(
                Pos2::new(plot.left() - 6.0, y),
                Align2::RIGHT_CENTER,
                format!("{f:.0}"),
                font.clone(),
                text,
            );
            f += f_step;
        }
        painter.text(
            Pos2::new(plot.right(), outer.bottom() - 4.0),
            Align2::RIGHT_BOTTOM,
            crate::i18n::tr("napięcie [mV]", "voltage [mV]"),
            font.clone(),
            text,
        );
        painter.text(
            Pos2::new(outer.left() + 6.0, outer.top() + 4.0),
            Align2::LEFT_TOP,
            "MHz",
            font.clone(),
            text,
        );

        // --- krzywe ---
        let clip = painter.with_clip_rect(plot.expand(4.0));
        let base: Vec<Pos2> = self
            .points
            .iter()
            .map(|p| Pos2::new(to_x(p.volt_mv()), to_y(p.base_mhz())))
            .collect();
        clip.add(Shape::line(
            base,
            Stroke::new(1.0_f32, Color32::from_gray(110)),
        ));

        let edited: Vec<Pos2> = self
            .points
            .iter()
            .map(|p| Pos2::new(to_x(p.volt_mv()), to_y(p.freq_mhz())))
            .collect();
        clip.add(Shape::line(edited.clone(), Stroke::new(2.0_f32, ACCENT)));

        // Zmienione punkty: biały na ciemnym tle, czarny na białym.
        let strong = ui.visuals().strong_text_color();
        for (i, pos) in edited.iter().enumerate() {
            let p = &self.points[i];
            let changed = p.delta_khz != p.applied_khz;
            let color = if changed { strong } else { ACCENT };
            if self.selected == Some(i) {
                clip.circle_filled(*pos, 5.5, color);
                clip.circle_stroke(*pos, 8.0, Stroke::new(1.5_f32, strong));
            } else if hover == Some(i) {
                clip.circle_filled(*pos, 5.0, color);
            } else {
                clip.circle_filled(*pos, 3.0, color);
            }
        }

        // --- bieżący punkt pracy ---
        if let Some((mv, mhz)) = live {
            if mv >= v_min && mv <= v_max {
                let s = Stroke::new(1.0_f32, crate::theme::warn());
                let (x, y) = (to_x(mv), to_y(mhz));
                clip.line_segment([Pos2::new(x, plot.top()), Pos2::new(x, plot.bottom())], s);
                clip.line_segment([Pos2::new(plot.left(), y), Pos2::new(plot.right(), y)], s);
            }
        }

        // --- opis punktu pod kursorem ---
        if let Some(i) = hover.or(self.dragging) {
            let p = &self.points[i];
            painter.text(
                Pos2::new(plot.right() - 6.0, plot.top() + 4.0),
                Align2::RIGHT_TOP,
                format!(
                    "{:.0} mV   {:.0} MHz   ({:+} MHz)",
                    p.volt_mv(),
                    p.freq_mhz(),
                    p.delta_khz / 1000
                ),
                FontId::proportional(13.0),
                ui.visuals().strong_text_color(),
            );
        }

        // --- kontrolki pod wykresem ---
        ui.add_space(6.0);
        ui.horizontal_wrapped(|ui| {
            if let Some(i) = self.selected.filter(|i| *i < self.points.len()) {
                let p = self.points[i];
                ui.label(crate::trf!("Punkt {} · {:.0} mV · fabrycznie {:.0} MHz · offset", "Point {} · {:.0} mV · stock {:.0} MHz · offset",
                    i + 1,
                    p.volt_mv(),
                    p.base_mhz()
                ));
                let mut mhz = p.delta_khz / 1000;
                let limit = MAX_DELTA_KHZ / 1000;
                if ui
                    .add(egui::DragValue::new(&mut mhz).range(-limit..=limit).suffix(" MHz"))
                    .changed()
                {
                    self.points[i].delta_khz = mhz * 1000;
                    self.flat_from = None;
                }
                ui.label(format!("→ {:.0} MHz", self.points[i].freq_mhz()));
                if ui
                    .button(crate::i18n::tr("Wyrównaj w prawo", "Flatten to the right"))
                    .on_hover_text(
                        crate::i18n::tr("Ustawia wszystkie punkty na prawo od zaznaczonego (wyższe napięcie) na jego częstotliwość. Krzywa robi się płaska od tego punktu (undervolting).", "Sets every point to the right of the selected one (higher voltage) to its frequency. The curve becomes flat from that point (undervolting)."),
                    )
                    .clicked()
                {
                    self.flatten_above(i);
                }
                if ui
                    .button(crate::i18n::tr("Ten sam offset wszędzie", "Same offset everywhere"))
                    .on_hover_text(crate::i18n::tr("Nadaje wszystkim punktom taki sam offset jak zaznaczony.", "Gives every point the same offset as the selected one."))
                    .clicked()
                {
                    self.same_offset_as(i);
                }
            } else {
                ui.label(
                    crate::i18n::tr("Kliknij punkt, aby go zaznaczyć. Przeciągnij w pionie, aby zmienić zegar. Shift + przeciąganie przesuwa całą krzywą.", "Click a point to select it. Drag vertically to change the clock. Shift + drag moves the whole curve."),
                );
            }
        });
        ui.horizontal_wrapped(|ui| {
            ui.label(crate::i18n::tr("Cała krzywa:", "Whole curve:"));
            for d in [-25, -5, 5, 25] {
                if ui.button(format!("{d:+} MHz")).clicked() {
                    self.shift_all(d * 1000);
                }
            }
            ui.separator();
            if ui.button(crate::i18n::tr("Wyzeruj offsety", "Reset offsets")).clicked() {
                self.zero_all();
            }
            if ui.add_enabled(self.dirty(), egui::Button::new(crate::i18n::tr("Cofnij edycję", "Undo edits"))).clicked() {
                self.revert();
            }
            if let Some(mv) = self.flat_voltage_mv() {
                ui.separator();
                ui.label(crate::trf!("Limit napięcia: {mv:.0} mV", "Voltage limit: {mv:.0} mV"))
                    .on_hover_text(
                        crate::i18n::tr("Krzywa jest wyrównana od tego punktu. Po każdym zapisie program sprawdza, czy żaden punkt o wyższym napięciu nie ma wyższego zegara.", "The curve is flat from this point. After each write the program checks that no higher-voltage point has a higher clock."),
                    );
            }
        });
    }
}
