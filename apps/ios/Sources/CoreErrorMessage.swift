import Foundation

func message(_ error: Error) -> String {
    if case let CoreError.Operation(detail) = error { return detail }
    return error.localizedDescription
}
