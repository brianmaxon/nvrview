import VideoKit

// Swift mirrors of the C interface's value types (docs/FFI.md, docs/nvrview.h).
// They carry no C types, so everything that is not a C call can be built and
// tested without the library. `CInterop.swift` converts to and from the C
// structs.

public struct PixelSize: Sendable, Equatable {
    public var width: UInt32
    public var height: UInt32
    public init(width: UInt32, height: UInt32) {
        self.width = width
        self.height = height
    }
    public static let zero = PixelSize(width: 0, height: 0)
}

/// Pixels, origin at the top left.
public struct PixelRect: Sendable, Equatable {
    public var x: UInt32
    public var y: UInt32
    public var width: UInt32
    public var height: UInt32
    public init(x: UInt32, y: UInt32, width: UInt32, height: UInt32) {
        self.x = x
        self.y = y
        self.width = width
        self.height = height
    }
    public var size: PixelSize { PixelSize(width: width, height: height) }
}

/// `NvStreamTier`.
public enum StreamTier: UInt32, Sendable, Equatable, CaseIterable {
    case main = 0
    case sub = 1
    case mobile = 2
    case extern = 4

    /// Parses `main`, `sub`, `mobile`, `extern` (command-line spelling).
    public init?(name: String) {
        switch name.lowercased() {
        case "main": self = .main
        case "sub": self = .sub
        case "mobile": self = .mobile
        case "extern": self = .extern
        default: return nil
        }
    }

    public var name: String {
        switch self {
        case .main: "main"
        case .sub: "sub"
        case .mobile: "mobile"
        case .extern: "extern"
        }
    }
}

/// `NvColorMatrix`.
public enum ColorMatrix: UInt32, Sendable, Equatable {
    case bt601 = 0
    case bt709 = 1
    case bt2020 = 2
}

/// `NvTileStatus`.
public enum TileStatus: UInt32, Sendable, Equatable {
    case waiting = 0
    case playing = 1
    case recovering = 2
    case undecodable = 3
}

/// `NvColor`: what the decoder must output and the matrix the shader applies.
public struct CoreColor: Sendable, Equatable {
    public var matrix: ColorMatrix
    public var fullRange: Bool
    /// Row-major 3x4, 12 floats: (R, G, B) = M * (Y, Cb, Cr, 1).
    public var ycbcrToRGB: [Float]

    public init(matrix: ColorMatrix, fullRange: Bool, ycbcrToRGB: [Float]) {
        self.matrix = matrix
        self.fullRange = fullRange
        self.ycbcrToRGB = ycbcrToRGB
    }
}

/// `NvDecodeFailure`, value for value. The platform decoder's OS status
/// codes have no place in the contract and stay in the app's log.
public enum CoreDecodeFailure: Int32, Sendable, Equatable, CaseIterable {
    case invalidParameterSets = 0
    case sessionCreateFailed = 1
    case notConfigured = 2
    case invalidAccessUnit = 3
    case submitFailed = 4
    case decodeError = 5
    case noPicture = 6

    public init(_ failure: DecodeFailure) {
        switch failure {
        case .invalidParameterSets: self = .invalidParameterSets
        case .sessionCreateFailed: self = .sessionCreateFailed
        case .notConfigured: self = .notConfigured
        case .invalidAccessUnit: self = .invalidAccessUnit
        case .submitFailed: self = .submitFailed
        case .decodeError: self = .decodeError
        case .noPicture: self = .noPicture
        }
    }

    /// Failures that answer a Configure rather than a Decode.
    public var answersConfigure: Bool {
        self == .invalidParameterSets || self == .sessionCreateFailed
    }
}

/// `NvStatus`.
public struct CoreStatus: Sendable, Equatable, Error {
    public var code: Int32
    public init(_ code: Int32) { self.code = code }

    public static let ok = CoreStatus(0)
    public static let invalidArgument = CoreStatus(-1)
    public static let unknownID = CoreStatus(-2)
    public static let closed = CoreStatus(-3)
    public static let limit = CoreStatus(-4)
    public static let `internal` = CoreStatus(-5)
    /// ABI 3: the output buffer is too small; the length needed came back.
    public static let bufferTooSmall = CoreStatus(-6)

    public var isOK: Bool { code == 0 }
}

/// One tile's cell, destination in the viewport, and source in the visible
/// picture (`NvTileLayout`).
public struct TileLayout: Sendable, Equatable {
    public var cell: PixelRect
    public var dest: PixelRect
    public var source: PixelRect
    public init(cell: PixelRect, dest: PixelRect, source: PixelRect) {
        self.cell = cell
        self.dest = dest
        self.source = source
    }
}

// MARK: Actions

/// `NvConfigure`. Everything here is BORROWED for the duration of the
/// callback that delivered it (the parameter set buffers point into the
/// core's memory); copy what you keep.
public struct CoreConfigure {
    public var codec: VideoCodec
    /// VPS, SPS, PPS order; NAL units without start codes.
    public var parameterSets: [UnsafeRawBufferPointer]
    public var codedSize: PixelSize
    public var visibleRect: PixelRect
    public var color: CoreColor

    public init(
        codec: VideoCodec, parameterSets: [UnsafeRawBufferPointer], codedSize: PixelSize,
        visibleRect: PixelRect, color: CoreColor
    ) {
        self.codec = codec
        self.parameterSets = parameterSets
        self.codedSize = codedSize
        self.visibleRect = visibleRect
        self.color = color
    }
}

/// `NvDecode`. The access unit is BORROWED, like `CoreConfigure`.
public struct CoreDecode {
    public var accessUnit: UnsafeRawBufferPointer
    public var keyframe: Bool
    public var ptsMicros: Int64

    public init(accessUnit: UnsafeRawBufferPointer, keyframe: Bool, ptsMicros: Int64) {
        self.accessUnit = accessUnit
        self.keyframe = keyframe
        self.ptsMicros = ptsMicros
    }
}

/// `NvAction`, one instruction for a tile's decoder or view.
public enum CoreAction {
    case configure(CoreConfigure)
    case decode(CoreDecode)
    case flush
    case reset
    case clear
    case status(TileStatus)
}
