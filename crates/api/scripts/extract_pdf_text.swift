import Foundation
import PDFKit

guard CommandLine.arguments.count == 2 else {
  FileHandle.standardError.write(Data("usage: extract_pdf_text.swift <file>\n".utf8))
  exit(2)
}

let url = URL(fileURLWithPath: CommandLine.arguments[1])
guard let document = PDFDocument(url: url) else {
  FileHandle.standardError.write(Data("could not open PDF\n".utf8))
  exit(2)
}

var chunks: [String] = []
for index in 0..<document.pageCount {
  if let text = document.page(at: index)?.string?.trimmingCharacters(in: .whitespacesAndNewlines), !text.isEmpty {
    chunks.append(text)
  }
}

print(chunks.joined(separator: "\n\n"))
