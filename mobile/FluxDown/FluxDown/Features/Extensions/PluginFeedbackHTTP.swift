import Foundation

#if FULL_FLAVOR
nonisolated enum PluginFeedbackHTTP {
    private static let endpoint = URL(string: "https://fluxdown.com/api/feedback")!
    static let fallback = URL(string: "https://fluxdown.com/feedback")!

    enum Failure: Error, Equatable {
        case unavailable, routeChanged, rateLimited, failed

        var messageKey: String {
            switch self {
            case .unavailable: "plugFeedbackUnavailable"
            case .routeChanged: "plugFeedbackRouteChanged"
            case .rateLimited: "feedbackRateLimited"
            case .failed: "plugFeedbackFailed"
            }
        }
    }

    nonisolated struct Report: Encodable, Sendable {
        let type = "bug"
        let source = "app"
        let title: String
        let description: String
        let appVersion: String
        let environment: String?
        let pluginIdentity: String
        let pluginVersion: String
        let repository: String
    }

    private struct Route: Decodable { let repository: String }
    private struct Receipt: Decodable { let issueNumber: Int }

    @concurrent
    static func repository(for identity: String) async throws -> String {
        var url = URLComponents(url: endpoint, resolvingAgainstBaseURL: false)!
        url.queryItems = [URLQueryItem(name: "pluginIdentity", value: identity)]
        url.percentEncodedQuery = url.percentEncodedQuery?.replacingOccurrences(of: "+", with: "%2B")
        guard let endpoint = url.url else { throw Failure.failed }
        let data = try await request(URLRequest(url: endpoint), expecting: 200)
        let route = try JSONDecoder().decode(Route.self, from: data)
        guard !route.repository.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else { throw Failure.failed }
        return route.repository
    }

    @concurrent
    static func submit(_ report: Report) async throws -> Int {
        var request = URLRequest(url: endpoint)
        request.httpMethod = "POST"
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        request.httpBody = try JSONEncoder().encode(report)
        let data = try await Self.request(request, expecting: 201)
        let receipt = try JSONDecoder().decode(Receipt.self, from: data)
        guard receipt.issueNumber > 0 else { throw Failure.failed }
        return receipt.issueNumber
    }

    @concurrent
    private static func request(_ request: URLRequest, expecting status: Int) async throws -> Data {
        let configuration = URLSessionConfiguration.ephemeral
        configuration.httpShouldSetCookies = false
        configuration.urlCredentialStorage = nil
        configuration.requestCachePolicy = .reloadIgnoringLocalCacheData
        let session = URLSession(configuration: configuration, delegate: NoRedirects(), delegateQueue: nil)
        defer { session.finishTasksAndInvalidate() }
        let (data, response) = try await session.data(for: request)
        guard let response = response as? HTTPURLResponse else { throw Failure.failed }
        switch response.statusCode {
        case status: return data
        case 404: throw Failure.unavailable
        case 409 where status == 201: throw Failure.routeChanged
        case 429: throw Failure.rateLimited
        default: throw Failure.failed
        }
    }

    /// Keep report data on the official endpoint even if it returns a redirect.
    private nonisolated final class NoRedirects: NSObject, URLSessionTaskDelegate {
        func urlSession(
            _ session: URLSession, task: URLSessionTask, willPerformHTTPRedirection response: HTTPURLResponse,
            newRequest request: URLRequest, completionHandler: @escaping @Sendable (URLRequest?) -> Void
        ) {
            completionHandler(nil)
        }
    }
}
#endif
