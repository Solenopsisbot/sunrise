use super::{DensityError, DensityVolume, NanRange, PerlinNoise, SimplexNoise, SmearedPerlinNoise};
use std::sync::Arc;

/// Immutable noise sampling. Implementations may override the default scalar
/// volume traversal when vanilla has a distinct bulk arithmetic path.
pub trait Noise: std::fmt::Debug + Send + Sync {
    fn range(&self) -> Result<[f32; 2], NanRange>;
    fn get2(&self, x: f64, z: f64) -> f32;
    fn get3(&self, x: f64, y: f64, z: f64) -> f32;
    fn add_to_volume(
        &self,
        buffer: &mut [f32],
        volume: DensityVolume,
        xz_scale: f64,
        y_scale: f64,
        amplitude: f32,
    ) -> Result<(), DensityError> {
        if buffer.len() < volume.size() {
            return Err(DensityError::BufferTooShort);
        }
        let [sx, sy, sz] = volume.sizes();
        let mut index = 0;
        for z in 0..sz {
            let z = f64::from(volume.coordinate(2, z)) * xz_scale;
            for x in 0..sx {
                let x = f64::from(volume.coordinate(0, x)) * xz_scale;
                for y in 0..sy {
                    let y = f64::from(volume.coordinate(1, y)) * y_scale;
                    buffer[index] += amplitude * self.get3(x, y, z);
                    index += 1;
                }
            }
        }
        Ok(())
    }
    /// [`Noise::add_to_volume`] for a caller that reads only the Ys set in `needed`: bit `y` of
    /// `needed[column]`, X/Z columns in buffer order. The other Ys may be left unchanged.
    fn add_to_needed(
        &self,
        buffer: &mut [f32],
        volume: DensityVolume,
        xz_scale: f64,
        y_scale: f64,
        amplitude: f32,
        _needed: &[u64],
    ) -> Result<(), DensityError> {
        self.add_to_volume(buffer, volume, xz_scale, y_scale, amplitude)
    }
}

impl Noise for PerlinNoise {
    fn range(&self) -> Result<[f32; 2], NanRange> {
        Ok(PerlinNoise::range(self))
    }
    fn get2(&self, x: f64, z: f64) -> f32 {
        PerlinNoise::get2(self, x, z)
    }
    fn get3(&self, x: f64, y: f64, z: f64) -> f32 {
        PerlinNoise::get3(self, x, y, z)
    }
    fn add_to_volume(
        &self,
        buffer: &mut [f32],
        volume: DensityVolume,
        xz_scale: f64,
        y_scale: f64,
        amplitude: f32,
    ) -> Result<(), DensityError> {
        PerlinNoise::add_to_volume(self, buffer, volume, xz_scale, y_scale, amplitude)
    }
    fn add_to_needed(
        &self,
        buffer: &mut [f32],
        volume: DensityVolume,
        xz_scale: f64,
        y_scale: f64,
        amplitude: f32,
        needed: &[u64],
    ) -> Result<(), DensityError> {
        PerlinNoise::add_to_needed(self, buffer, volume, xz_scale, y_scale, amplitude, needed)
    }
}
impl Noise for SmearedPerlinNoise {
    fn range(&self) -> Result<[f32; 2], NanRange> {
        SmearedPerlinNoise::range(self)
    }
    fn get2(&self, x: f64, z: f64) -> f32 {
        SmearedPerlinNoise::get2(self, x, z)
    }
    fn get3(&self, x: f64, y: f64, z: f64) -> f32 {
        SmearedPerlinNoise::get3(self, x, y, z)
    }
    fn add_to_volume(
        &self,
        buffer: &mut [f32],
        volume: DensityVolume,
        xz_scale: f64,
        y_scale: f64,
        amplitude: f32,
    ) -> Result<(), DensityError> {
        SmearedPerlinNoise::add_to_volume(self, buffer, volume, xz_scale, y_scale, amplitude)
    }
    fn add_to_needed(
        &self,
        buffer: &mut [f32],
        volume: DensityVolume,
        xz_scale: f64,
        y_scale: f64,
        amplitude: f32,
        needed: &[u64],
    ) -> Result<(), DensityError> {
        SmearedPerlinNoise::add_to_needed(
            self, buffer, volume, xz_scale, y_scale, amplitude, needed,
        )
    }
}
impl Noise for SimplexNoise {
    fn range(&self) -> Result<[f32; 2], NanRange> {
        Ok(SimplexNoise::range(self))
    }
    fn get2(&self, x: f64, z: f64) -> f32 {
        SimplexNoise::get2(self, x, z)
    }
    fn get3(&self, x: f64, y: f64, z: f64) -> f32 {
        SimplexNoise::get3(self, x, y, z)
    }
}

#[derive(Clone, Debug)]
enum DispatchNoise {
    Perlin(Arc<PerlinNoise>),
    SmearedPerlin(Arc<SmearedPerlinNoise>),
    Simplex(Arc<SimplexNoise>),
    Dynamic(Arc<dyn Noise>),
}

impl DispatchNoise {
    #[inline]
    fn get2(&self, x: f64, z: f64) -> f32 {
        match self {
            Self::Perlin(n) => n.get2(x, z),
            Self::SmearedPerlin(n) => n.get2(x, z),
            Self::Simplex(n) => n.get2(x, z),
            Self::Dynamic(n) => n.get2(x, z),
        }
    }
    #[inline]
    fn get3(&self, x: f64, y: f64, z: f64) -> f32 {
        match self {
            Self::Perlin(n) => n.get3(x, y, z),
            Self::SmearedPerlin(n) => n.get3(x, y, z),
            Self::Simplex(n) => n.get3(x, y, z),
            Self::Dynamic(n) => n.get3(x, y, z),
        }
    }
    #[inline]
    fn add_to_volume(
        &self,
        buffer: &mut [f32],
        volume: DensityVolume,
        xz_scale: f64,
        y_scale: f64,
        amplitude: f32,
    ) -> Result<(), DensityError> {
        match self {
            Self::Perlin(n) => n.add_to_volume(buffer, volume, xz_scale, y_scale, amplitude),
            Self::SmearedPerlin(n) => n.add_to_volume(buffer, volume, xz_scale, y_scale, amplitude),
            Self::Simplex(n) => {
                Noise::add_to_volume(n.as_ref(), buffer, volume, xz_scale, y_scale, amplitude)
            }
            Self::Dynamic(n) => n.add_to_volume(buffer, volume, xz_scale, y_scale, amplitude),
        }
    }
    #[inline]
    fn add_to_needed(
        &self,
        buffer: &mut [f32],
        volume: DensityVolume,
        xz_scale: f64,
        y_scale: f64,
        amplitude: f32,
        needed: &[u64],
    ) -> Result<(), DensityError> {
        match self {
            Self::Perlin(n) => {
                n.add_to_needed(buffer, volume, xz_scale, y_scale, amplitude, needed)
            }
            Self::SmearedPerlin(n) => {
                n.add_to_needed(buffer, volume, xz_scale, y_scale, amplitude, needed)
            }
            Self::Simplex(n) => Noise::add_to_needed(
                n.as_ref(),
                buffer,
                volume,
                xz_scale,
                y_scale,
                amplitude,
                needed,
            ),
            Self::Dynamic(n) => {
                n.add_to_needed(buffer, volume, xz_scale, y_scale, amplitude, needed)
            }
        }
    }
}

#[derive(Clone, Debug)]
struct DispatchLayer {
    noise: DispatchNoise,
    frequency: f64,
    amplitude: f32,
}

#[derive(Clone, Debug)]
pub struct NoiseLayer {
    pub noise: Arc<dyn Noise>,
    pub frequency: f64,
    pub amplitude: f32,
}

#[derive(Clone, Debug)]
pub struct NoiseStack {
    layers: Vec<NoiseLayer>,
    dispatch: Vec<DispatchLayer>,
    range: [f32; 2],
}

impl NoiseStack {
    pub fn builder() -> NoiseStackBuilder {
        NoiseStackBuilder::default()
    }
    pub fn layers(&self) -> &[NoiseLayer] {
        &self.layers
    }
    pub fn get_layer(&self, index: usize) -> Option<&Arc<dyn Noise>> {
        self.layers.get(index).map(|layer| &layer.noise)
    }
}
impl Noise for NoiseStack {
    fn range(&self) -> Result<[f32; 2], NanRange> {
        Ok(self.range)
    }
    fn get2(&self, x: f64, z: f64) -> f32 {
        let mut value = 0.0;
        for layer in &self.dispatch {
            value += layer.amplitude * layer.noise.get2(x * layer.frequency, z * layer.frequency);
        }
        value
    }
    fn get3(&self, x: f64, y: f64, z: f64) -> f32 {
        let mut value = 0.0;
        for layer in &self.dispatch {
            value += layer.amplitude
                * layer.noise.get3(
                    x * layer.frequency,
                    y * layer.frequency,
                    z * layer.frequency,
                );
        }
        value
    }
    fn add_to_volume(
        &self,
        buffer: &mut [f32],
        volume: DensityVolume,
        xz_scale: f64,
        y_scale: f64,
        amplitude: f32,
    ) -> Result<(), DensityError> {
        self.add_layers(buffer, volume, [xz_scale, y_scale], amplitude, None)
    }
    fn add_to_needed(
        &self,
        buffer: &mut [f32],
        volume: DensityVolume,
        xz_scale: f64,
        y_scale: f64,
        amplitude: f32,
        needed: &[u64],
    ) -> Result<(), DensityError> {
        self.add_layers(buffer, volume, [xz_scale, y_scale], amplitude, Some(needed))
    }
}
impl NoiseStack {
    fn add_layers(
        &self,
        buffer: &mut [f32],
        volume: DensityVolume,
        [xz_scale, y_scale]: [f64; 2],
        amplitude: f32,
        needed: Option<&[u64]>,
    ) -> Result<(), DensityError> {
        if buffer.len() < volume.size() {
            return Err(DensityError::BufferTooShort);
        }
        for layer in &self.dispatch {
            let (xz_scale, y_scale) = (xz_scale * layer.frequency, y_scale * layer.frequency);
            let amplitude = amplitude * layer.amplitude;
            match needed {
                Some(needed) => layer
                    .noise
                    .add_to_needed(buffer, volume, xz_scale, y_scale, amplitude, needed)?,
                None => layer
                    .noise
                    .add_to_volume(buffer, volume, xz_scale, y_scale, amplitude)?,
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Default)]
pub struct NoiseStackBuilder {
    layers: Vec<NoiseLayer>,
    dispatch: Vec<DispatchLayer>,
}
impl NoiseStackBuilder {
    pub fn add(&mut self, noise: Arc<dyn Noise>, frequency: f64, amplitude: f32) -> &mut Self {
        self.dispatch.push(DispatchLayer {
            noise: DispatchNoise::Dynamic(Arc::clone(&noise)),
            frequency,
            amplitude,
        });
        self.layers.push(NoiseLayer {
            noise,
            frequency,
            amplitude,
        });
        self
    }
    pub fn add_perlin(&mut self, noise: PerlinNoise, frequency: f64, amplitude: f32) -> &mut Self {
        let arc = Arc::new(noise);
        self.dispatch.push(DispatchLayer {
            noise: DispatchNoise::Perlin(Arc::clone(&arc)),
            frequency,
            amplitude,
        });
        self.layers.push(NoiseLayer {
            noise: arc,
            frequency,
            amplitude,
        });
        self
    }
    pub fn add_smeared_perlin(
        &mut self,
        noise: SmearedPerlinNoise,
        frequency: f64,
        amplitude: f32,
    ) -> &mut Self {
        let arc = Arc::new(noise);
        self.dispatch.push(DispatchLayer {
            noise: DispatchNoise::SmearedPerlin(Arc::clone(&arc)),
            frequency,
            amplitude,
        });
        self.layers.push(NoiseLayer {
            noise: arc,
            frequency,
            amplitude,
        });
        self
    }
    pub fn add_simplex(
        &mut self,
        noise: SimplexNoise,
        frequency: f64,
        amplitude: f32,
    ) -> &mut Self {
        let arc = Arc::new(noise);
        self.dispatch.push(DispatchLayer {
            noise: DispatchNoise::Simplex(Arc::clone(&arc)),
            frequency,
            amplitude,
        });
        self.layers.push(NoiseLayer {
            noise: arc,
            frequency,
            amplitude,
        });
        self
    }
    /// Flattening rescales each existing layer before subsequent sampling.
    pub fn add_stack(&mut self, stack: &NoiseStack, frequency: f64, amplitude: f32) -> &mut Self {
        self.layers
            .extend(stack.layers.iter().map(|layer| NoiseLayer {
                noise: Arc::clone(&layer.noise),
                frequency: layer.frequency * frequency,
                amplitude: layer.amplitude * amplitude,
            }));
        self.dispatch
            .extend(stack.dispatch.iter().map(|layer| DispatchLayer {
                noise: layer.noise.clone(),
                frequency: layer.frequency * frequency,
                amplitude: layer.amplitude * amplitude,
            }));
        self
    }
    pub fn build(&self) -> Result<NoiseStack, NanRange> {
        let mut range = [0.0; 2];
        for layer in &self.layers {
            let bounds = layer.noise.range()?;
            if layer.amplitude.is_nan() {
                return Err(NanRange);
            }
            let scaled = if bounds[0].is_nan() || bounds[1].is_nan() {
                [f32::NAN; 2]
            } else {
                // Interval.mulBound defines zero times infinity as positive zero.
                let product = |v: f32| {
                    if v == 0.0 || layer.amplitude == 0.0 {
                        0.0
                    } else {
                        v * layer.amplitude
                    }
                };
                let a = product(bounds[0]);
                let b = product(bounds[1]);
                if a < b {
                    [a, b]
                } else if a > b {
                    [b, a]
                } else if a == 0.0 {
                    [
                        f32::from_bits(a.to_bits() | b.to_bits()),
                        f32::from_bits(a.to_bits() & b.to_bits()),
                    ]
                } else {
                    [a, b]
                }
            };
            range = [range[0] + scaled[0], range[1] + scaled[1]];
            if range[0].is_nan() || range[1].is_nan() {
                range = [f32::NAN; 2];
            }
            // Interval.of interns [0,1], including a supplied negative zero.
            if range[0] == 0.0 && range[1] == 1.0 {
                range[0] = 0.0;
            }
        }
        Ok(NoiseStack {
            layers: self.layers.clone(),
            dispatch: self.dispatch.clone(),
            range,
        })
    }
}
