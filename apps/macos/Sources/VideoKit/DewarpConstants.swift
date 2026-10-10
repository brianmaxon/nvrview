/// The shader constants of one dewarped fisheye tile (docs/RENDERING.md,
/// "Shader constants"): 22 rows of four floats, 352 bytes, the same layout
/// as `DewarpConstants` in `gridShaderSource` and the Windows app's HLSL
/// constant buffer. NVRCore computes every row (`FisheyeDewarp.constants`);
/// the renderer only binds them.
public struct DewarpConstants: Sendable, Equatable {
    /// One view: a rectangle of the tile and its projection.
    public struct View: Sendable, Equatable {
        /// Left, top, right, bottom in the tile (0...1).
        public var rect: SIMD4<Float> = .zero
        /// xyz an axis in the lens frame; w the projection (0 rectilinear,
        /// 1 cylindrical).
        public var axisX: SIMD4<Float> = .zero
        /// xyz an axis; w tan(h/2) (rectilinear) or ψ0 (cylindrical).
        public var axisY: SIMD4<Float> = .zero
        /// xyz an axis; w tan(v/2) (rectilinear) or Δψ (cylindrical).
        public var axisZ: SIMD4<Float> = .zero
        /// Cylindrical: τ0, Δτ.
        public var extra: SIMD4<Float> = .zero

        public init() {}
    }

    public static let maxViews = 4

    /// cx, cy, r, r·W/H: the image circle in picture fractions.
    public var lens: SIMD4<Float> = .zero
    /// θmax (radians), number of views.
    public var params: SIMD4<Float> = .zero
    public var view0 = View()
    public var view1 = View()
    public var view2 = View()
    public var view3 = View()

    public init() {}

    public subscript(view i: Int) -> View {
        get {
            switch i {
            case 0: view0
            case 1: view1
            case 2: view2
            default: view3
            }
        }
        set {
            switch i {
            case 0: view0 = newValue
            case 1: view1 = newValue
            case 2: view2 = newValue
            default: view3 = newValue
            }
        }
    }
}
